//! Opt-in local control socket for the real GPUI window.
//!
//! Nothing listens until [`start`] is called, and [`start`] binds a filesystem
//! Unix-domain socket only — never a TCP port, and never an abstract-namespace
//! socket. Call it only after the user explicitly enables automation: a client
//! that can connect can read private conversations and inject input.
//!
//! The parent directory must already exist, must not be a symlink, and must be
//! mode `0700` owned by the current user. That private directory is what makes
//! path checks trustworthy. On Linux, `bind` creates a filesystem socket inode
//! distinct from the listening file descriptor; `fchmod` on the listening
//! descriptor does not restrict the path `connect` checks. Mode is set with
//! `fchmodat2(AT_EMPTY_PATH)` on an `O_PATH|O_NOFOLLOW` descriptor for the
//! bound inode, so a symlink swapped onto the path is not followed. The socket
//! path itself must not exist. An existing file, socket, or symlink is left
//! untouched, including when a previous process crashed and left a stale socket.
//!
//! Frames are one JSON object per line, UTF-8, at most 64 KiB, and a complete
//! line must arrive within 5 seconds:
//!
//! ```text
//! {"command":"key","key":"enter"}
//! {"command":"text","text":"hello"}
//! {"command":"click","x":12.0,"y":4.0}
//! {"command":"screenshot","path":"/tmp/hermes.png"}
//! {"command":"state"}
//! ```
//!
//! Each complete response is one JSON object and a newline. The UI's reply is
//! forwarded unchanged. EOF, a timeout, or a partial reply after sending a
//! command means its outcome is unknown; do not retry it as if it did not run.
//!
//! A command the UI has not [`Request::claim`]ed is cancelled. The client is
//! told it was not applied, and the UI must not apply it. A command the UI has
//! already claimed, but not answered, is
//! `{"ok":false,"ambiguous":true,"error":"..."}`. That means the effect may
//! have happened; it is not a failure to retry as if nothing ran. Reply writes
//! share one deadline with shutdown and do not use `write_all`.
//!
//! The GPUI foreground thread should poll [`Automation::requests`] and send
//! exactly one JSON object on [`Request::reply`] after a successful claim.
//! After the listener stops, `try_recv` returns
//! [`std::sync::mpsc::TryRecvError::Disconnected`].

use std::ffi::{CStr, CString};
use std::io::{Error, ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

const MAX_REQUEST_BYTES: usize = 64 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
const ACCEPT_IDLE: Duration = Duration::from_millis(50);
const PARENT_MODE: u32 = 0o700;
const SOCKET_MODE: u32 = 0o600;

// Linux `<asm-generic/socket.h>`. Used only to reject other users after the
// filesystem checks; this is not a TCP socket option set.
const SOL_SOCKET: i32 = 1;
const SO_PEERCRED: i32 = 17;

// Linux open/fcntl flags. `O_NOFOLLOW` applies only to the final component.
const O_DIRECTORY: i32 = 0o200000;
const O_NOFOLLOW: i32 = 0o400000;
const O_CLOEXEC: i32 = 0o2000000;
const O_PATH: i32 = 0o10000000;
const AT_FDCWD: i32 = -100;
const AT_EMPTY_PATH: i32 = 0x1000;
// glibc's `fchmodat(AT_SYMLINK_NOFOLLOW)` is not used: on kernels without
// `fchmodat2` it can fall back to `chmod(2)`, which follows a final symlink.
// `SYS_fchmodat2` is the asm-generic number (x86_64 and aarch64).
const SYS_FCHMODAT2: i64 = 452;
const RENAME_NOREPLACE: u32 = 1;

const ADMIT_PENDING: u8 = 0;
const ADMIT_CLAIMED: u8 = 1;
const ADMIT_CANCELLED: u8 = 2;
unsafe extern "C" {
    fn geteuid() -> u32;
    fn getsockopt(
        sockfd: i32,
        level: i32,
        optname: i32,
        optval: *mut std::ffi::c_void,
        optlen: *mut u32,
    ) -> i32;
    fn openat(dirfd: i32, path: *const i8, flags: i32, mode: u32) -> i32;
    fn unlinkat(dirfd: i32, path: *const i8, flags: i32) -> i32;
    fn renameat2(
        olddirfd: i32,
        oldpath: *const i8,
        newdirfd: i32,
        newpath: *const i8,
        flags: u32,
    ) -> i32;
    fn syscall(number: i64, ...) -> i64;
}

/// Listener handle. Poll `requests` on the GPUI foreground thread.
///
/// Dropping the handle stops the listener and unlinks the bound socket on the
/// dropping thread. The listener is detached: joining could deadlock if this
/// drop runs on the GPUI thread while the listener is blocked in a reply wait
/// this drop will never complete. Unlink renames the directory entry aside and
/// deletes it only when that entry is still the bound inode. A replacement is
/// restored or left in place, never deleted.
#[must_use = "dropping Automation closes the local socket"]
#[derive(Debug)]
pub struct Automation {
    pub requests: mpsc::Receiver<Request>,
    shutdown: Arc<AtomicBool>,
    listener: Option<std::thread::JoinHandle<()>>,
    bound: Arc<BoundSocket>,
}

/// One command for the GPUI foreground thread.
///
/// [`claim`](Request::claim) before applying the command. A false claim means
/// the listener already told the client the command was not applied; applying
/// it would make that response a lie. After a successful claim, send exactly
/// one JSON object on `reply`: `{"ok":true,...}` when the command was applied,
/// or `{"ok":false,"error":"..."}` when it was not. The listener writes that
/// object as a single newline-terminated line. A reply that arrives after the
/// client was already answered finds the sender disconnected; ignore that error.
#[derive(Debug)]
pub struct Request {
    pub command: Command,
    pub reply: mpsc::Sender<Value>,
    admission: Arc<Admission>,
}

impl Request {
    /// Reserve this command for execution.
    ///
    /// Returns false when the listener already abandoned it. The effect must
    /// not be applied in that case.
    pub fn claim(&self) -> bool {
        self.admission.claim()
    }
}

/// Commands accepted on the socket. Unknown fields and unknown commands are rejected.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Key { key: String },
    Text { text: String },
    Click { x: f32, y: f32 },
    Screenshot { path: String },
    State,
}

#[derive(Clone, Copy, Debug)]
struct Inode {
    dev: u64,
    ino: u64,
}

struct Prepared {
    path: PathBuf,
    parent: PathBuf,
    parent_inode: Inode,
}

#[derive(Debug)]
struct BoundSocket {
    dir: OwnedFd,
    name: CString,
    inode: Inode,
    lock: Mutex<()>,
}

impl BoundSocket {
    fn unlink(&self) {
        let _guard = self.lock.lock().unwrap_or_else(|poison| poison.into_inner());
        unlink_if_same_inode(self.dir.as_raw_fd(), &self.name, self.inode);
    }
}

impl Drop for BoundSocket {
    fn drop(&mut self) {
        self.unlink();
    }
}

impl Drop for Automation {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Unlink here. The listener is detached below, and a clean process
        // exit does not run a detached thread's destructors.
        self.bound.unlink();
        // Detach. Joining would deadlock if this drop runs on the GPUI thread
        // while the listener is waiting for a reply this drop will never send.
        self.listener.take();
    }
}

/// Bind an owner-only Unix socket at `path` and start accepting commands.
///
/// This is the opt-in. No socket exists until this returns `Ok`. The parent
/// directory must be mode `0700`, owned by the current user, and not a symlink.
/// `path` must not already exist; this function never deletes an existing
/// socket or symlink.
pub fn start(path: &Path) -> Result<Automation, String> {
    start_with(path, REPLY_TIMEOUT)
}

fn start_with(path: &Path, reply_timeout: Duration) -> Result<Automation, String> {
    let prepared = prepare(path)?;
    let listener = match UnixListener::bind(&prepared.path) {
        Ok(listener) => listener,
        Err(error) => {
            return Err(format!(
                "Could not bind the automation socket at {}: {error}",
                prepared.path.display()
            ));
        }
    };
    let bound = seal(&prepared)?;
    if let Err(error) = listener.set_nonblocking(true) {
        return Err(format!(
            "Could not listen on the automation socket: {error}"
        ));
    }
    let bound = Arc::new(bound);
    let (tx, rx) = mpsc::channel();
    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&shutdown);
    let thread_bound = Arc::clone(&bound);
    let listener = match std::thread::Builder::new()
        .name("hermes-automation".into())
        .spawn(move || serve(listener, thread_bound, tx, flag, reply_timeout))
    {
        Ok(handle) => handle,
        Err(error) => {
            return Err(format!(
                "Could not start the automation listener: {error}"
            ));
        }
    };
    Ok(Automation {
        requests: rx,
        shutdown,
        listener: Some(listener),
        bound,
    })
}

fn prepare(path: &Path) -> Result<Prepared, String> {
    let name = socket_name(path)?;
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let (canonical, parent_inode) = resolve_private_parent(parent)?;
    let socket = canonical.join(name);
    ensure_absent(&socket)?;
    Ok(Prepared {
        path: socket,
        parent: canonical,
        parent_inode,
    })
}

fn socket_name(path: &Path) -> Result<&std::ffi::OsStr, String> {
    match path.file_name() {
        Some(name) if !name.is_empty() && name != "." && name != ".." => Ok(name),
        _ => Err("The automation socket path needs a file name.".into()),
    }
}

fn resolve_private_parent(parent: &Path) -> Result<(PathBuf, Inode), String> {
    let listed = inspect_parent(parent)?;
    let canonical = std::fs::canonicalize(parent)
        .map_err(|error| format!("Could not resolve the automation socket directory: {error}"))?;
    let resolved = inspect_parent(&canonical)?;
    if resolved.dev() != listed.dev() || resolved.ino() != listed.ino() {
        return Err("The automation socket directory changed while it was checked.".into());
    }
    Ok((
        canonical,
        Inode {
            dev: resolved.dev(),
            ino: resolved.ino(),
        },
    ))
}

fn inspect_parent(parent: &Path) -> Result<std::fs::Metadata, String> {
    let meta = std::fs::symlink_metadata(parent).map_err(|error| {
        format!("Could not inspect the automation socket directory: {error}")
    })?;
    if meta.file_type().is_symlink() {
        return Err("The automation socket directory must not be a symlink.".into());
    }
    if !meta.is_dir() {
        return Err("The automation socket path's parent is not a directory.".into());
    }
    if meta.uid() != current_euid() {
        return Err(
            "The automation socket directory must be owned by the current user.".into(),
        );
    }
    let mode = meta.mode() & 0o777;
    if mode != PARENT_MODE {
        return Err(format!(
            "The automation socket directory must be private (mode 0700, found {mode:04o})."
        ));
    }
    Ok(meta)
}

fn ensure_absent(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(
            "Refusing to replace a symlink at the automation socket path.".into(),
        ),
        Ok(meta) if meta.file_type().is_socket() => Err(format!(
            "Refusing to replace an existing socket at {}. \
             Remove it manually if a previous process crashed.",
            path.display()
        )),
        Ok(_) => Err(
            "Refusing to replace an existing file at the automation socket path.".into(),
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Could not inspect the automation socket path: {error}"
        )),
    }
}

fn seal(prepared: &Prepared) -> Result<BoundSocket, String> {
    // Hold the directory we checked. Later chmod and unlink use this fd, so a
    // replaced parent path cannot redirect them.
    let dir = open_directory(&prepared.parent)?;
    directory_still_private(&dir, prepared.parent_inode)?;
    let name = c_file_name(&prepared.path)?;
    let inode = restrict_socket(dir.as_raw_fd(), &name)?;
    if let Err(error) = directory_still_private(&dir, prepared.parent_inode) {
        unlink_if_same_inode(dir.as_raw_fd(), &name, inode);
        return Err(error);
    }
    if !name_is_restricted(dir.as_raw_fd(), &name, inode) {
        unlink_if_same_inode(dir.as_raw_fd(), &name, inode);
        return Err(
            "The automation socket path was replaced before listening started.".into(),
        );
    }
    Ok(BoundSocket {
        dir,
        name,
        inode,
        lock: Mutex::new(()),
    })
}

fn open_directory(path: &Path) -> Result<OwnedFd, String> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "The automation socket directory path is not usable.".to_string())?;
    let fd = unsafe {
        openat(
            AT_FDCWD,
            c_path.as_ptr(),
            O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(format!(
            "Could not open the automation socket directory: {}",
            Error::last_os_error()
        ));
    }
    // SAFETY: openat just returned this fd, and it is not owned elsewhere.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn directory_still_private(dir: &OwnedFd, expected: Inode) -> Result<(), String> {
    let meta = metadata_fd(dir).map_err(|error| {
        format!("Could not inspect the automation socket directory: {error}")
    })?;
    if !meta.is_dir()
        || meta.dev() != expected.dev
        || meta.ino() != expected.ino
        || meta.uid() != current_euid()
        || meta.mode() & 0o777 != PARENT_MODE
    {
        return Err(
            "The automation socket directory changed before listening started.".into(),
        );
    }
    Ok(())
}

fn c_file_name(path: &Path) -> Result<CString, String> {
    let name = path
        .file_name()
        .ok_or_else(|| "The automation socket path needs a file name.".to_string())?;
    CString::new(name.as_bytes())
        .map_err(|_| "The automation socket path is not usable.".to_string())
}

fn restrict_socket(dirfd: RawFd, name: &CStr) -> Result<Inode, String> {
    let opened = open_nofollow(dirfd, name).map_err(|error| {
        format!("Could not inspect the automation socket: {error}")
    })?;
    let meta = metadata_fd(&opened).map_err(|error| {
        format!("Could not inspect the automation socket: {error}")
    })?;
    let inode = owned_socket_inode(&meta)?;
    // Chmod the inode this fd already names. `chmod` on the path would follow
    // a symlink swapped in after the check above.
    if let Err(error) = chmod_owned_inode(&opened) {
        unlink_if_same_inode(dirfd, name, inode);
        return Err(format!("Could not restrict the automation socket: {error}"));
    }
    confirm_restricted(dirfd, name, &opened, inode)
}

fn owned_socket_inode(meta: &std::fs::Metadata) -> Result<Inode, String> {
    if meta.file_type().is_symlink() {
        return Err(
            "The automation socket path was replaced by a symlink before listening started."
                .into(),
        );
    }
    if !meta.file_type().is_socket() {
        return Err(
            "The automation socket path was replaced before listening started.".into(),
        );
    }
    if meta.uid() != current_euid() {
        return Err("The automation socket is not owned by the current user.".into());
    }
    Ok(Inode {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

fn confirm_restricted(
    dirfd: RawFd,
    name: &CStr,
    opened: &OwnedFd,
    inode: Inode,
) -> Result<Inode, String> {
    let meta = match metadata_fd(opened) {
        Ok(meta) => meta,
        Err(error) => {
            unlink_if_same_inode(dirfd, name, inode);
            return Err(format!("Could not inspect the automation socket: {error}"));
        }
    };
    if !same_socket(&meta, inode)
        || meta.uid() != current_euid()
        || meta.mode() & 0o777 != SOCKET_MODE
    {
        unlink_if_same_inode(dirfd, name, inode);
        return Err(
            "Could not restrict the automation socket to owner-only access.".into(),
        );
    }
    Ok(inode)
}

fn chmod_owned_inode(fd: &OwnedFd) -> Result<(), Error> {
    let empty = b"\0";
    // SAFETY: `fd` is open for the duration of the call. The empty path is
    // NUL-terminated and not retained. `AT_EMPTY_PATH` makes fchmodat2 operate
    // on that descriptor rather than a looked-up path. The syscall number is
    // Linux `fchmodat2`.
    let rc = unsafe {
        syscall(
            SYS_FCHMODAT2,
            fd.as_raw_fd() as i64,
            empty.as_ptr() as i64,
            SOCKET_MODE as i64,
            AT_EMPTY_PATH as i64,
        )
    };
    if rc < 0 {
        Err(Error::last_os_error())
    } else {
        Ok(())
    }
}

fn name_is_restricted(dirfd: RawFd, name: &CStr, inode: Inode) -> bool {
    let Ok(opened) = open_nofollow(dirfd, name) else {
        return false;
    };
    let Ok(meta) = metadata_fd(&opened) else {
        return false;
    };
    same_socket(&meta, inode)
        && meta.uid() == current_euid()
        && meta.mode() & 0o777 == SOCKET_MODE
}

fn open_nofollow(dirfd: RawFd, name: &CStr) -> Result<OwnedFd, Error> {
    let fd = io_retry(|| unsafe { openat(dirfd, name.as_ptr(), O_PATH | O_NOFOLLOW | O_CLOEXEC, 0) });
    if fd < 0 {
        return Err(Error::last_os_error());
    }
    // SAFETY: openat just returned this fd, and it is not owned elsewhere.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn metadata_fd(fd: &OwnedFd) -> Result<std::fs::Metadata, Error> {
    let cloned = fd.try_clone()?;
    std::fs::File::from(cloned).metadata()
}

fn unlink_if_same_inode(dirfd: RawFd, name: &CStr, inode: Inode) {
    // Never `unlink` the public name after a separate lstat. That deletes a
    // replacement created between the check and the unlink. Rename the entry
    // aside, then delete the temporary name only if it is still our inode.
    // A displaced replacement is renamed back, or left under the temporary
    // name if the public name was taken again. It is not deleted.
    let Ok(opened) = open_nofollow(dirfd, name) else {
        return;
    };
    let Ok(meta) = metadata_fd(&opened) else {
        return;
    };
    if !same_socket(&meta, inode) {
        return;
    }
    after_matching_inode();
    let tmp = temp_name(inode);
    if !rename_noreplace(dirfd, name, &tmp) {
        return;
    }
    let displaced_is_ours = open_nofollow(dirfd, &tmp)
        .and_then(|fd| metadata_fd(&fd))
        .map(|meta| same_socket(&meta, inode))
        .unwrap_or(false);
    if displaced_is_ours {
        let _ = unlink_at(dirfd, &tmp);
        return;
    }
    if open_nofollow(dirfd, name).is_err() {
        let _ = rename_noreplace(dirfd, &tmp, name);
    }
}

fn temp_name(inode: Inode) -> CString {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    CString::new(format!(
        ".hermes-unlink-{}-{}-{}-{n}-{nanos}",
        std::process::id(),
        inode.dev,
        inode.ino,
    ))
    .expect("unlink temporary name")
}

fn rename_noreplace(dirfd: RawFd, from: &CStr, to: &CStr) -> bool {
    io_retry(|| unsafe { renameat2(dirfd, from.as_ptr(), dirfd, to.as_ptr(), RENAME_NOREPLACE) })
        == 0
}

fn unlink_at(dirfd: RawFd, name: &CStr) -> bool {
    io_retry(|| unsafe { unlinkat(dirfd, name.as_ptr(), 0) }) == 0
}

fn io_retry(mut op: impl FnMut() -> i32) -> i32 {
    loop {
        let rc = op();
        if rc < 0 && Error::last_os_error().kind() == ErrorKind::Interrupted {
            continue;
        }
        return rc;
    }
}

fn same_socket(meta: &std::fs::Metadata, inode: Inode) -> bool {
    meta.file_type().is_socket() && meta.dev() == inode.dev && meta.ino() == inode.ino
}

#[cfg(test)]
fn after_matching_inode() {
    tests::park_unlink_if_armed();
}

#[cfg(not(test))]
fn after_matching_inode() {}

fn serve(
    listener: UnixListener,
    bound: Arc<BoundSocket>,
    tx: mpsc::Sender<Request>,
    shutdown: Arc<AtomicBool>,
    reply_timeout: Duration,
) {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                handle_client(stream, &tx, &shutdown, reply_timeout);
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => std::thread::sleep(ACCEPT_IDLE),
        }
    }
    drop(bound);
}

fn handle_client(
    stream: UnixStream,
    tx: &mpsc::Sender<Request>,
    shutdown: &AtomicBool,
    reply_timeout: Duration,
) {
    if let Err(error) = configure(&stream).and_then(|()| ensure_same_user(&stream)) {
        let _ = write_json(&stream, &rejected(&error), shutdown);
        return;
    }
    let mut buffer = Vec::new();
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        let line = match read_line(&stream, &mut buffer, shutdown) {
            Ok(Some(line)) => line,
            Ok(None) => return,
            Err(error) => {
                let _ = write_json(&stream, &rejected(&error), shutdown);
                return;
            }
        };
        match run_command(&line, tx, shutdown, reply_timeout) {
            Ok(value) => {
                if write_json(&stream, &value, shutdown).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = write_json(&stream, &rejected(&error), shutdown);
                return;
            }
        }
    }
}

fn configure(stream: &UnixStream) -> Result<(), String> {
    stream.set_nonblocking(false).map_err(|error| {
        format!("Could not configure the automation connection: {error}")
    })?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).map_err(|error| {
        format!("Could not configure the automation connection: {error}")
    })?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT)).map_err(|error| {
        format!("Could not configure the automation connection: {error}")
    })?;
    Ok(())
}

fn ensure_same_user(stream: &UnixStream) -> Result<(), String> {
    #[repr(C)]
    struct Ucred {
        pid: i32,
        uid: u32,
        gid: u32,
    }
    let mut cred = Ucred {
        pid: 0,
        uid: u32::MAX,
        gid: 0,
    };
    let mut len = std::mem::size_of::<Ucred>() as u32;
    // SAFETY: `cred` and `len` live for the call. SO_PEERCRED writes a
    // `ucred` for this connected Unix stream and does not retain the pointer.
    let rc = unsafe {
        getsockopt(
            stream.as_raw_fd(),
            SOL_SOCKET,
            SO_PEERCRED,
            (&mut cred as *mut Ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(format!(
            "Could not verify the automation client: {}",
            Error::last_os_error()
        ));
    }
    if len < 8 {
        return Err("Could not verify the automation client.".into());
    }
    let uid = current_euid();
    if cred.uid != uid {
        return Err(format!(
            "Refusing an automation connection from user {}.",
            cred.uid
        ));
    }
    Ok(())
}

fn read_line(
    stream: &UnixStream,
    buffer: &mut Vec<u8>,
    shutdown: &AtomicBool,
) -> Result<Option<Vec<u8>>, String> {
    let deadline = Instant::now() + READ_TIMEOUT;
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return Err("Automation stopped.".into());
        }
        if let Some(line) = split_line(buffer)? {
            return Ok(Some(line));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            if buffer.is_empty() {
                return Ok(None);
            }
            return Err("Timed out waiting for a request.".into());
        }
        let slice = remaining
            .min(Duration::from_millis(200))
            .max(Duration::from_millis(1));
        if !read_request_chunk(stream, buffer, slice)? {
            return Ok(None);
        }
    }
}

// `Ok(false)` means the peer closed before any request bytes arrived.
fn read_request_chunk(
    mut stream: &UnixStream,
    buffer: &mut Vec<u8>,
    slice: Duration,
) -> Result<bool, String> {
    stream
        .set_read_timeout(Some(slice))
        .map_err(|error| format!("Could not time out the automation socket: {error}"))?;
    let mut chunk = [0u8; 2048];
    match stream.read(&mut chunk) {
        Ok(0) if buffer.is_empty() => Ok(false),
        Ok(0) => Err("The connection closed before a complete request.".into()),
        Ok(n) => {
            buffer.extend_from_slice(&chunk[..n]);
            Ok(true)
        }
        Err(error) if retryable(error.kind()) => Ok(true),
        Err(error) => Err(format!("Could not read a request: {error}")),
    }
}

fn retryable(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::Interrupted | ErrorKind::TimedOut | ErrorKind::WouldBlock
    )
}

fn split_line(buffer: &mut Vec<u8>) -> Result<Option<Vec<u8>>, String> {
    if let Some(index) = buffer.iter().position(|byte| *byte == b'\n') {
        if index > MAX_REQUEST_BYTES {
            return Err("Request is too long.".into());
        }
        let mut line: Vec<u8> = buffer.drain(..=index).collect();
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        return Ok(Some(line));
    }
    if buffer.len() > MAX_REQUEST_BYTES {
        return Err("Request is too long.".into());
    }
    Ok(None)
}

fn run_command(
    line: &[u8],
    tx: &mpsc::Sender<Request>,
    shutdown: &AtomicBool,
    reply_timeout: Duration,
) -> Result<Value, String> {
    let command = match parse_command(line) {
        Ok(command) => command,
        Err(error) => return Ok(rejected(&error)),
    };
    if shutdown.load(Ordering::SeqCst) {
        return Err("Automation stopped.".into());
    }
    let (reply_tx, reply_rx) = mpsc::channel();
    let admission = Admission::new();
    tx.send(Request {
        command,
        reply: reply_tx,
        admission: Arc::clone(&admission),
    })
    .map_err(|_| "The UI is not accepting automation requests.".to_string())?;
    wait_reply(&reply_rx, shutdown, &admission, reply_timeout)
}

fn parse_command(bytes: &[u8]) -> Result<Command, String> {
    if bytes.is_empty() {
        return Err("The request was empty.".into());
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("Could not parse the command: {error}"))?;
    if value.get("command").and_then(Value::as_str) == Some("state")
        && value.as_object().is_some_and(|object| object.len() != 1)
    {
        return Err("The state command accepts no additional fields.".into());
    }
    let command: Command = serde_json::from_value(value)
        .map_err(|error| format!("Could not parse the command: {error}"))?;
    validate(&command)?;
    Ok(command)
}

fn validate(command: &Command) -> Result<(), String> {
    match command {
        Command::Key { key } if key.is_empty() => {
            Err("The key command needs a key name.".into())
        }
        Command::Text { text } if text.is_empty() => {
            Err("The text command needs text.".into())
        }
        Command::Click { x, y } if !x.is_finite() || !y.is_finite() => {
            Err("Click coordinates must be finite.".into())
        }
        Command::Screenshot { path } if path.is_empty() => {
            Err("The screenshot command needs a path.".into())
        }
        _ => Ok(()),
    }
}

#[derive(Debug)]
struct Admission {
    state: AtomicU8,
}

impl Admission {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU8::new(ADMIT_PENDING),
        })
    }

    fn claim(&self) -> bool {
        self.state
            .compare_exchange(
                ADMIT_PENDING,
                ADMIT_CLAIMED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    fn cancel(&self) -> bool {
        self.state
            .compare_exchange(
                ADMIT_PENDING,
                ADMIT_CANCELLED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    fn is_claimed(&self) -> bool {
        self.state.load(Ordering::SeqCst) == ADMIT_CLAIMED
    }
}

fn wait_reply(
    rx: &mpsc::Receiver<Value>,
    shutdown: &AtomicBool,
    admission: &Admission,
    reply_timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + reply_timeout;
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return abandon(
                rx,
                admission,
                "Automation stopped before the command was accepted; it was not applied.",
                "Automation stopped while the command was in flight. It may have been applied.",
            );
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return abandon(
                rx,
                admission,
                "The UI did not accept the command in time; it was not applied.",
                "The command may have been applied; the UI did not answer in time.",
            );
        }
        let slice = remaining
            .min(Duration::from_millis(50))
            .max(Duration::from_millis(1));
        match rx.recv_timeout(slice) {
            Ok(value) => return Ok(value),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return abandon(
                    rx,
                    admission,
                    "The UI did not answer; the command was not applied.",
                    "The UI dropped the command after accepting it. It may have been applied.",
                );
            }
        }
    }
}

fn abandon(
    rx: &mpsc::Receiver<Value>,
    admission: &Admission,
    not_applied: &str,
    in_flight: &str,
) -> Result<Value, String> {
    if let Some(value) = pending_reply(rx) {
        return Ok(value);
    }
    if admission.cancel() {
        if let Some(value) = pending_reply(rx) {
            return Ok(value);
        }
        return Err(not_applied.into());
    }
    if let Some(value) = pending_reply(rx) {
        return Ok(value);
    }
    if admission.is_claimed() {
        Ok(ambiguous(in_flight))
    } else {
        Err(not_applied.into())
    }
}

fn pending_reply(rx: &mpsc::Receiver<Value>) -> Option<Value> {
    rx.try_recv().ok()
}

fn write_json(stream: &UnixStream, value: &Value, shutdown: &AtomicBool) -> Result<(), String> {
    write_reply(stream, value, shutdown, WRITE_TIMEOUT)
}

fn write_reply(
    stream: &UnixStream,
    value: &Value,
    shutdown: &AtomicBool,
    budget: Duration,
) -> Result<(), String> {
    let bytes = encode_reply_line(value)?;
    let mut bytes = bytes.as_slice();
    // `write_all` plus `SO_SNDTIMEO` is not a total deadline: each successful
    // short write starts the timeout over, and shutdown is not observed.
    let deadline = Instant::now() + budget;
    while !bytes.is_empty() {
        let wrote = write_reply_chunk(stream, bytes, shutdown, deadline)?;
        bytes = &bytes[wrote..];
    }
    finish_reply(stream, shutdown, deadline)
}

fn encode_reply_line(value: &Value) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec(value)
        .map_err(|_| "Could not encode the reply.".to_string())?;
    if bytes.iter().any(|byte| *byte == b'\n' || *byte == b'\r') {
        return Err("Could not encode the reply as one line.".into());
    }
    bytes.push(b'\n');
    Ok(bytes)
}

fn write_reply_chunk(
    mut stream: &UnixStream,
    bytes: &[u8],
    shutdown: &AtomicBool,
    deadline: Instant,
) -> Result<usize, String> {
    if shutdown.load(Ordering::SeqCst) {
        return Err("Automation stopped.".into());
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err("Timed out writing the reply.".into());
    }
    set_reply_timeout(stream, remaining)?;
    match stream.write(bytes) {
        Ok(0) => Err("Could not write the reply.".into()),
        Ok(n) => Ok(n),
        Err(error) if retryable(error.kind()) => Ok(0),
        Err(error) => Err(format!("Could not write the reply: {error}")),
    }
}

fn finish_reply(
    mut stream: &UnixStream,
    shutdown: &AtomicBool,
    deadline: Instant,
) -> Result<(), String> {
    if shutdown.load(Ordering::SeqCst) {
        return Err("Automation stopped.".into());
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err("Timed out writing the reply.".into());
    }
    set_reply_timeout(stream, remaining)?;
    stream
        .flush()
        .map_err(|error| format!("Could not write the reply: {error}"))?;
    Ok(())
}

fn reply_write_timeout(remaining: Duration) -> Duration {
    remaining
        .min(Duration::from_millis(200))
        .max(Duration::from_millis(1))
}

fn set_reply_timeout(stream: &UnixStream, remaining: Duration) -> Result<(), String> {
    stream
        .set_write_timeout(Some(reply_write_timeout(remaining)))
        .map_err(|error| format!("Could not time out the automation socket: {error}"))
}

fn ambiguous(error: &str) -> Value {
    json!({"ok": false, "ambiguous": true, "error": error})
}

fn rejected(error: &str) -> Value {
    json!({"ok": false, "error": error})
}

fn current_euid() -> u32 {
    // SAFETY: geteuid has no preconditions and only reads the process euid.
    unsafe { geteuid() }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    thread_local! {
        static UNLINK_PARK: RefCell<Option<UnlinkPark>> = const { RefCell::new(None) };
    }

    struct UnlinkPark {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    pub(super) fn park_unlink_if_armed() {
        let park = UNLINK_PARK.with(|slot| slot.borrow_mut().take());
        if let Some(park) = park {
            let _ = park.entered.send(());
            let _ = park.release.recv_timeout(Duration::from_secs(2));
        }
    }

    fn scratch() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hermes-automation-{}-{}-{n}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn next_request(auto: &Automation) -> Request {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match auto.requests.try_recv() {
                Ok(request) => return request,
                Err(mpsc::TryRecvError::Empty) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("no request: {error}"),
            }
        }
    }

    fn exchange(path: &Path, request: &str) -> String {
        let mut stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.flush().unwrap();
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            stream.read_exact(&mut byte).unwrap();
            if byte[0] == b'\n' {
                break;
            }
            line.push(byte[0]);
        }
        String::from_utf8(line).unwrap()
    }

    fn inode_of(path: &Path) -> Inode {
        let meta = std::fs::symlink_metadata(path).unwrap();
        Inode {
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }

    fn fill_socket(mut stream: &UnixStream) {
        stream.set_nonblocking(true).unwrap();
        let chunk = vec![0u8; 8192];
        loop {
            match stream.write(&chunk) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error)
                    if error.kind() == ErrorKind::WouldBlock
                        || error.kind() == ErrorKind::TimedOut =>
                {
                    break;
                }
                Err(error) => panic!("{error}"),
            }
        }
        stream.set_nonblocking(false).unwrap();
    }

    #[test]
    fn parses_each_command_and_rejects_garbage() {
        assert_eq!(
            parse_command(br#"{"command":"key","key":"enter"}"#).unwrap(),
            Command::Key { key: "enter".into() }
        );
        assert_eq!(
            parse_command(br#"{"command":"text","text":"hi"}"#).unwrap(),
            Command::Text { text: "hi".into() }
        );
        assert_eq!(
            parse_command(br#"{"command":"click","x":1.5,"y":2}"#).unwrap(),
            Command::Click { x: 1.5, y: 2.0 }
        );
        assert_eq!(
            parse_command(br#"{"command":"screenshot","path":"/tmp/a.png"}"#).unwrap(),
            Command::Screenshot {
                path: "/tmp/a.png".into()
            }
        );
        assert_eq!(
            parse_command(br#"{"command":"state"}"#).unwrap(),
            Command::State
        );
        assert!(parse_command(br#"{"command":"Key","key":"enter"}"#).is_err());
        assert!(parse_command(br#"{"command":"state","extra":1}"#).is_err());
        assert!(parse_command(br#"{"command":"key","key":""}"#).is_err());
        assert!(parse_command(br#"{"command":"text","text":""}"#).is_err());
        assert!(parse_command(br#"{"command":"screenshot","path":""}"#).is_err());
        assert!(validate(&Command::Click {
            x: f32::NAN,
            y: 1.0
        })
        .is_err());
        assert!(validate(&Command::Click {
            x: 1.0,
            y: f32::INFINITY
        })
        .is_err());
    }

    #[test]
    fn split_line_bounds_length_and_keeps_the_tail() {
        let mut buffer = b"{\"command\":\"state\"}\r\n{\"command\":\"key\"".to_vec();
        let line = split_line(&mut buffer).unwrap().unwrap();
        assert_eq!(line, b"{\"command\":\"state\"}");
        assert_eq!(buffer, b"{\"command\":\"key\"");

        let mut huge = vec![b'a'; MAX_REQUEST_BYTES + 1];
        assert!(split_line(&mut huge).unwrap_err().contains("too long"));
    }

    #[test]
    fn unlink_decision_keeps_symlinks_and_foreign_inodes() {
        let dir = scratch();
        let _keep = Temp(dir.clone());
        let target = dir.join("target");
        std::fs::write(&target, b"keep").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let dirfd = open_directory(&dir).unwrap();
        let link_name = CString::new("link").unwrap();
        let link_meta = std::fs::symlink_metadata(&link).unwrap();
        unlink_if_same_inode(
            dirfd.as_raw_fd(),
            &link_name,
            Inode {
                dev: link_meta.dev(),
                ino: link_meta.ino(),
            },
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");

        let first = dir.join("a.sock");
        let second = dir.join("b.sock");
        let _listener_a = UnixListener::bind(&first).unwrap();
        let _listener_b = UnixListener::bind(&second).unwrap();
        let first_name = CString::new("a.sock").unwrap();
        unlink_if_same_inode(dirfd.as_raw_fd(), &first_name, inode_of(&second));
        assert!(std::fs::symlink_metadata(&first).unwrap().file_type().is_socket());

        unlink_if_same_inode(dirfd.as_raw_fd(), &first_name, inode_of(&first));
        assert!(std::fs::symlink_metadata(&first).is_err());
        assert!(std::fs::symlink_metadata(&second).unwrap().file_type().is_socket());
    }

    #[test]
    fn unlink_after_a_matching_stat_does_not_delete_a_replacement() {
        let dir = scratch();
        let _keep = Temp(dir.clone());
        let sock = dir.join("a.sock");
        let _listener = UnixListener::bind(&sock).unwrap();
        let original = inode_of(&sock);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        UNLINK_PARK.with(|slot| {
            *slot.borrow_mut() = Some(UnlinkPark {
                entered: entered_tx,
                release: release_rx,
            });
        });
        let dirfd = open_directory(&dir).unwrap();
        let name = CString::new("a.sock").unwrap();
        let sock_for_replace = sock.clone();
        let replacer = std::thread::spawn(move || {
            entered_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("unlink did not reach the inode check");
            std::fs::remove_file(&sock_for_replace).unwrap();
            let replacement = UnixListener::bind(&sock_for_replace).unwrap();
            let inode = inode_of(&sock_for_replace);
            release_tx.send(()).unwrap();
            (replacement, inode)
        });
        unlink_if_same_inode(dirfd.as_raw_fd(), &name, original);
        let (replacement, inode) = replacer.join().unwrap();
        let meta = std::fs::symlink_metadata(&sock).unwrap();
        assert!(meta.file_type().is_socket());
        assert_eq!(meta.ino(), inode.ino);
        assert_ne!(meta.ino(), original.ino);
        drop(replacement);
    }

    #[test]
    fn restrict_does_not_chmod_through_a_symlink() {
        let dir = scratch();
        let _keep = Temp(dir.clone());
        let secret = dir.join("secret");
        std::fs::write(&secret, b"keep").unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::os::unix::fs::symlink(&secret, dir.join("link")).unwrap();
        let dirfd = open_directory(&dir).unwrap();
        let name = CString::new("link").unwrap();
        let error = restrict_socket(dirfd.as_raw_fd(), &name).unwrap_err();
        assert!(
            error.contains("symlink") || error.contains("replaced") || error.contains("socket"),
            "{error}"
        );
        assert_eq!(
            std::fs::metadata(&secret).unwrap().mode() & 0o777,
            0o644,
            "chmod followed the symlink"
        );
    }

    #[test]
    fn start_leaves_existing_socket_file_and_symlink_untouched() {
        let dir = scratch();
        let _keep = Temp(dir.clone());

        let sock = dir.join("already.sock");
        drop(UnixListener::bind(&sock).unwrap());
        let before = std::fs::symlink_metadata(&sock).unwrap();
        let error = start(&sock).unwrap_err();
        assert!(error.contains("Refusing to replace an existing socket"), "{error}");
        let after = std::fs::symlink_metadata(&sock).unwrap();
        assert_eq!(before.ino(), after.ino());
        assert!(after.file_type().is_socket());

        let file = dir.join("notes");
        std::fs::write(&file, b"private").unwrap();
        let error = start(&file).unwrap_err();
        assert!(error.contains("Refusing to replace an existing file"), "{error}");
        assert_eq!(std::fs::read(&file).unwrap(), b"private");

        let link = dir.join("linked");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        let error = start(&link).unwrap_err();
        assert!(error.contains("symlink"), "{error}");
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read(&file).unwrap(), b"private");
    }

    #[test]
    fn start_refuses_a_public_or_symlinked_parent_without_binding() {
        let dir = scratch();
        let _keep = Temp(dir.clone());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let sock = dir.join("open.sock");
        let error = start(&sock).unwrap_err();
        assert!(error.contains("0700"), "{error}");
        assert!(std::fs::symlink_metadata(&sock).is_err());

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let real = dir.join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        let linked = dir.join("linked-parent");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        let error = start(&linked.join("sock")).unwrap_err();
        assert!(error.contains("symlink"), "{error}");
        assert!(std::fs::symlink_metadata(&linked).unwrap().file_type().is_symlink());
        assert!(std::fs::symlink_metadata(real.join("sock")).is_err());
    }

    #[test]
    fn serves_newline_json_and_does_not_unlink_a_substituted_path() {
        let dir = scratch();
        let _keep = Temp(dir.clone());
        let sock = dir.join("agent.sock");
        let auto = start(&sock).unwrap();
        let meta = std::fs::symlink_metadata(&sock).unwrap();
        assert!(meta.file_type().is_socket());
        assert_eq!(meta.mode() & 0o777, 0o600);
        assert_eq!(
            std::fs::symlink_metadata(&dir).unwrap().mode() & 0o777,
            0o700
        );

        let path = sock.clone();
        let client = std::thread::spawn(move || {
            exchange(&path, "{\"command\":\"click\",\"x\":3,\"y\":4}\n")
        });
        let request = next_request(&auto);
        assert_eq!(
            request.command,
            Command::Click { x: 3.0, y: 4.0 }
        );
        assert!(request.claim());
        request
            .reply
            .send(serde_json::json!({"ok": true, "dispatched": true}))
            .unwrap();
        let reply: Value = serde_json::from_str(&client.join().unwrap()).unwrap();
        assert_eq!(reply, serde_json::json!({"ok": true, "dispatched": true}));

        let bad = exchange(&sock, "{\"command\":\"nope\"}\n");
        let parsed: Value = serde_json::from_str(&bad).unwrap();
        assert_eq!(parsed["ok"], false);
        assert!(parsed["error"].as_str().unwrap().contains("parse"), "{bad}");
        assert!(matches!(
            auto.requests.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        std::fs::remove_file(&sock).unwrap();
        std::fs::write(&sock, b"substituted").unwrap();
        drop(auto);
        assert_eq!(std::fs::read(&sock).unwrap(), b"substituted");
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            assert_eq!(std::fs::read(&sock).unwrap(), b"substituted");
        }
    }

    #[test]
    fn drop_unlinks_the_socket_without_waiting_for_the_listener() {
        let dir = scratch();
        let _keep = Temp(dir.clone());
        let sock = dir.join("agent.sock");
        let auto = start(&sock).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let client = loop {
            if let Ok(stream) = UnixStream::connect(&sock) {
                break stream;
            }
            if Instant::now() >= deadline {
                panic!("could not connect");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        drop(auto);
        assert!(
            std::fs::symlink_metadata(&sock).is_err(),
            "clean drop left the socket behind"
        );
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().starts_with(".hermes-unlink-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        drop(client);
    }

    #[test]
    fn drop_keeps_a_replacement_socket() {
        let dir = scratch();
        let _keep = Temp(dir.clone());
        let sock = dir.join("agent.sock");
        let auto = start(&sock).unwrap();
        std::fs::remove_file(&sock).unwrap();
        let replacement = UnixListener::bind(&sock).unwrap();
        let inode = std::fs::symlink_metadata(&sock).unwrap().ino();
        drop(auto);
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            let meta = std::fs::symlink_metadata(&sock).unwrap();
            assert!(meta.file_type().is_socket());
            assert_eq!(meta.ino(), inode);
        }
        drop(replacement);
    }

    #[test]
    fn queued_command_is_not_applied_after_a_definite_timeout() {
        let dir = scratch();
        let _keep = Temp(dir.clone());
        let sock = dir.join("agent.sock");
        let auto = start_with(&sock, Duration::from_millis(250)).unwrap();
        let path = sock.clone();
        let client = std::thread::spawn(move || exchange(&path, "{\"command\":\"state\"}\n"));
        let request = next_request(&auto);
        let reply = client.join().unwrap();
        let parsed: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(parsed["ok"], false, "{reply}");
        assert!(parsed.get("ambiguous").is_none(), "{reply}");
        let error = parsed["error"].as_str().unwrap();
        assert!(error.contains("was not applied"), "{error}");
        assert!(
            !error.contains("may have been applied"),
            "{error}"
        );
        assert!(
            !request.claim(),
            "a timed-out unclaimed command must not be executable"
        );
    }

    #[test]
    fn claimed_command_timeout_is_ambiguous() {
        let dir = scratch();
        let _keep = Temp(dir.clone());
        let sock = dir.join("agent.sock");
        let auto = start_with(&sock, Duration::from_millis(400)).unwrap();
        let path = sock.clone();
        let client = std::thread::spawn(move || exchange(&path, "{\"command\":\"state\"}\n"));
        let request = next_request(&auto);
        assert!(request.claim());
        let reply = client.join().unwrap();
        let parsed: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(parsed["ok"], false, "{reply}");
        assert_eq!(parsed["ambiguous"], true, "{reply}");
        let error = parsed["error"].as_str().unwrap();
        assert!(error.contains("may have been applied"), "{error}");
        assert!(!error.contains("was not applied"), "{error}");
    }

    #[test]
    fn write_deadline_does_not_extend_when_the_peer_reads_slowly() {
        let (writer, mut reader) = UnixStream::pair().unwrap();
        fill_socket(&writer);
        let shutdown = AtomicBool::new(false);
        let payload = json!({"ok": true, "pad": "x".repeat(64 * 1024)});
        let started = Instant::now();
        let reader = std::thread::spawn(move || {
            let mut byte = [0u8; 1];
            let _ = reader.set_read_timeout(Some(Duration::from_millis(20)));
            for _ in 0..30 {
                let _ = reader.read(&mut byte);
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let error = write_reply(&writer, &payload, &shutdown, Duration::from_millis(200))
            .unwrap_err();
        let elapsed = started.elapsed();
        assert!(
            error.contains("Timed out"),
            "{error} after {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(800),
            "partial progress extended the deadline to {elapsed:?}"
        );
        drop(reader);
    }

    #[test]
    fn write_observes_shutdown_instead_of_blocking() {
        let (writer, reader) = UnixStream::pair().unwrap();
        fill_socket(&writer);
        let shutdown = AtomicBool::new(true);
        let started = Instant::now();
        let error = write_reply(
            &writer,
            &json!({"ok": false, "error": "late"}),
            &shutdown,
            Duration::from_secs(30),
        )
        .unwrap_err();
        assert!(error.contains("stopped"), "{error}");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "shutdown did not interrupt the write"
        );
        drop(reader);
    }
}
