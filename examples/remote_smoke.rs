use std::time::{Duration, Instant};

use hermes_native::backend::{self, Backend};
use hermes_native::model::{
    ChatMessage, Command, Config, ConnectionState, Event, Role, TurnStatus,
};

const WORD: &str = "lattice";
const OTHER: &str = "marble";
const TURN_ONE: &str = "Reply with exactly the single word lattice. Do not use tools.";
const TURN_TWO: &str = "What single word did I just ask you to reply with? Answer with that word only. Do not use tools.";
const TURN_OTHER: &str = "Reply with exactly the single word marble. Do not use tools.";

struct Harness {
    backend: Backend,
    events: Vec<Event>,
}

fn main() {
    let _ = dotenvy::from_path(".env");
    if !std::env::args().any(|argument| argument == "--trusted-tunnel") {
        eprintln!("config: pass --trusted-tunnel to confirm the encrypted tunnel.");
        std::process::exit(1);
    }
    let config = match Config::from_env(true) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("config: {error}");
            std::process::exit(1);
        }
    };
    let mut harness = Harness {
        backend: backend::spawn(config),
        events: Vec::new(),
    };
    let mut created = Vec::new();
    let result = run(&mut harness, &mut created);
    let cleanup = cleanup(&mut harness, &created);
    match (result, cleanup) {
        (Ok(proof), Ok(())) => println!("{proof}"),
        (Err(error), Ok(())) => {
            eprintln!("smoke failed: {error}");
            std::process::exit(1);
        }
        (result, Err(error)) => {
            if let Err(error) = result {
                eprintln!("smoke failed: {error}");
            }
            eprintln!("cleanup failed: {error}");
            for id in &created {
                eprintln!("disposable-id {id}");
            }
            std::process::exit(1);
        }
    }
}

fn run(harness: &mut Harness, created: &mut Vec<String>) -> Result<String, String> {
    if TURN_TWO.to_ascii_lowercase().contains(WORD)
        || TURN_OTHER.to_ascii_lowercase().contains(WORD)
        || OTHER.to_ascii_lowercase().contains(WORD)
    {
        return Err("later prompts must not contain the first chat's word".into());
    }
    wait_connected(harness, Duration::from_secs(40))?;
    let first = create(harness, "hn-smoke-a")?;
    created.push(first.0.clone());
    let second = create(harness, "hn-smoke-b")?;
    created.push(second.0.clone());
    let first_reply = submit(harness, &first.1, TURN_ONE)?;
    if first_reply.trim() != WORD {
        return Err(format!(
            "first reply was not the expected word (len {})",
            first_reply.trim().len()
        ));
    }
    let second_reply = submit(harness, &first.1, TURN_TWO)?;
    if second_reply.trim() != WORD {
        return Err(format!(
            "second reply lost context (len {})",
            second_reply.trim().len()
        ));
    }
    let other_reply = submit(harness, &second.1, TURN_OTHER)?;
    if other_reply.trim() != OTHER {
        return Err(format!(
            "second chat reply was not its own word (len {})",
            other_reply.trim().len()
        ));
    }
    let saved = resume(harness, &second.0)?;
    assert_separate(&saved.1, "the second chat")?;
    let resumed = resume(harness, &first.0)?;
    assert_context(&resumed.1)?;
    harness.send(Command::Reconnect)?;
    wait_connected(harness, Duration::from_secs(40))?;
    let cold_first = resume(harness, &first.0)?;
    let cold_second = resume(harness, &second.0)?;
    assert_context(&cold_first.1)?;
    assert_separate(&cold_second.1, "cold resume of the second chat")?;
    Ok(format!(
        "connected created=2 turn1={WORD} turn2_context=true second={OTHER} switched=true reconnect_history=true"
    ))
}

fn cleanup(harness: &mut Harness, created: &[String]) -> Result<(), String> {
    let mut failed = Vec::new();
    for id in created {
        if let Err(reason) = delete_one(harness, id) {
            failed.push(format!("{id}: {reason}"));
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} disposable chat(s) were not deleted: {}",
            failed.len(),
            failed.join("; ")
        ))
    }
}

fn delete_one(harness: &mut Harness, id: &str) -> Result<(), String> {
    if harness
        .send(Command::Delete {
            stored_id: id.to_string(),
        })
        .is_err()
    {
        return Err("delete was not sent".into());
    }
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(40) {
        if let Some(event) = harness.backend.try_recv() {
            match event {
                Event::Deleted { stored_id } if stored_id == id => return Ok(()),
                Event::Error {
                    operation,
                    session_id,
                    message,
                } if operation == "delete" && session_id.as_deref() == Some(id) => {
                    return Err(message);
                }
                event => harness.events.push(event),
            }
        } else {
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    Err("timed out".into())
}

fn create(harness: &mut Harness, title: &str) -> Result<(String, String), String> {
    harness.send(Command::Create {
        title: title.into(),
    })?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        for event in harness.drain() {
            match event {
                Event::Created {
                    session,
                    runtime_id,
                } if session.title == title => {
                    return Ok((session.stored_id, runtime_id));
                }
                Event::Error {
                    operation, message, ..
                } if operation == "create" => {
                    return Err(message);
                }
                event => harness.events.push(event),
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err("create timed out".into())
}

fn submit(harness: &mut Harness, runtime_id: &str, text: &str) -> Result<String, String> {
    harness.send(Command::Submit {
        runtime_id: runtime_id.into(),
        text: text.into(),
    })?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(180) {
        for event in harness.drain() {
            match event {
                Event::MessageComplete {
                    runtime_id: id,
                    message,
                    status,
                } if id == runtime_id => {
                    if status != TurnStatus::Complete {
                        return Err(format!("turn ended as {status:?}"));
                    }
                    return Ok(message.text);
                }
                Event::Error {
                    operation,
                    session_id,
                    message,
                } if session_id.as_deref() == Some(runtime_id)
                    && matches!(operation.as_str(), "submit" | "gateway") =>
                {
                    return Err(message);
                }
                event => harness.events.push(event),
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err("turn timed out; the prompt was not sent again".into())
}

fn resume(harness: &mut Harness, stored_id: &str) -> Result<(String, Vec<ChatMessage>), String> {
    harness.send(Command::Resume {
        stored_id: stored_id.into(),
    })?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(40) {
        for event in harness.drain() {
            match event {
                Event::Resumed {
                    stored_id: id,
                    runtime_id,
                    messages,
                    read_only_reason,
                    ..
                } if id == stored_id => {
                    if read_only_reason.is_some() {
                        return Err("smoke chat was refused as read-only".into());
                    }
                    let Some(runtime_id) = runtime_id else {
                        return Err("resume did not return a runtime id".into());
                    };
                    return Ok((runtime_id, messages));
                }
                Event::Error {
                    operation,
                    session_id,
                    message,
                } if operation == "resume" && session_id.as_deref() == Some(stored_id) => {
                    return Err(message);
                }
                event => harness.events.push(event),
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err("resume timed out".into())
}

fn assert_context(messages: &[ChatMessage]) -> Result<(), String> {
    let mut stage = 0u8;
    for message in messages {
        let text = message.text.trim();
        match (stage, &message.role, text) {
            (0, Role::User, text) if text == TURN_ONE => stage = 1,
            (1, Role::Assistant, text) if text == WORD => stage = 2,
            (2, Role::User, text) if text == TURN_TWO => stage = 3,
            (3, Role::Assistant, text) if text == WORD => stage = 4,
            (_, Role::User, text)
                if text.to_ascii_lowercase().contains(WORD) && text != TURN_ONE =>
            {
                return Err("stored prompt contained the secret word".into());
            }
            (_, _, text)
                if text.to_ascii_lowercase().contains(OTHER) || text.contains(TURN_OTHER) =>
            {
                return Err("the first chat contained the second chat's word".into());
            }
            _ => {}
        }
    }
    if stage < 4 {
        return Err(format!(
            "history was not in chronological context order (stage {stage})"
        ));
    }
    Ok(())
}

fn assert_separate(messages: &[ChatMessage], when: &str) -> Result<(), String> {
    if messages.iter().any(|message| {
        message.text.to_ascii_lowercase().contains(WORD) || message.text.contains(TURN_ONE)
    }) {
        return Err(format!("{when} contained the first chat's word"));
    }
    let mut stage = 0u8;
    for message in messages {
        let text = message.text.trim();
        match (stage, &message.role, text) {
            (0, Role::User, text) if text == TURN_OTHER => stage = 1,
            (1, Role::Assistant, text) if text == OTHER => stage = 2,
            _ => {}
        }
    }
    if stage < 2 {
        return Err(format!("{when} did not keep its own saved word"));
    }
    Ok(())
}

fn wait_connected(harness: &mut Harness, timeout: Duration) -> Result<(), String> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        for event in harness.drain() {
            match &event {
                Event::Connection(ConnectionState::Connected) => return Ok(()),
                Event::Connection(
                    ConnectionState::AuthenticationFailed(message)
                    | ConnectionState::Disconnected(message),
                ) => {
                    return Err(message.clone());
                }
                _ => harness.events.push(event),
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err("timed out waiting to connect".into())
}

impl Harness {
    fn send(&self, command: Command) -> Result<(), String> {
        self.backend.send(command)
    }

    fn drain(&mut self) -> Vec<Event> {
        let mut batch = std::mem::take(&mut self.events);
        while let Some(event) = self.backend.try_recv() {
            batch.push(event);
        }
        batch
    }
}
