pub mod backend;
pub mod inspector;
pub mod markdown;
pub mod model;
pub mod palette;
pub mod recall;
pub mod studio;
pub mod ui;

#[cfg(test)]
type GtkTestJob = Box<dyn FnOnce() + Send + 'static>;

#[cfg(test)]
fn gtk_test_sender() -> &'static std::sync::mpsc::SyncSender<GtkTestJob> {
    static SENDER: std::sync::OnceLock<std::sync::mpsc::SyncSender<GtkTestJob>> =
        std::sync::OnceLock::new();
    SENDER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<GtkTestJob>(0);
        std::thread::spawn(move || {
            let ready = gtk::init().is_ok();
            if ready {
                let _ = adw::init();
            }
            for job in rx {
                if ready {
                    job();
                }
            }
        });
        tx
    })
}

#[cfg(test)]
pub(crate) fn with_gtk_test_thread(run: impl FnOnce() + Send + 'static) {
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    gtk_test_sender()
        .send(Box::new(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
            let _ = done_tx.send(outcome);
        }))
        .expect("gtk test thread alive");
    if let Ok(Err(payload)) = done_rx.recv() {
        std::panic::resume_unwind(payload);
    }
}
