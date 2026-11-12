use std::time::{Duration, Instant};

use hermes_native::backend::{self, Backend};
use hermes_native::model::{
    Catalog, ChatMessage, Command, Config, ConnectionState, Event, ModelIdentity, RewindMode, Role,
    SearchHit, SeedMessage, SessionMutation, SessionSummary, TurnStatus,
};

const WORD: &str = "lattice";
const OTHER: &str = "marble";
const TURN_ONE: &str = "Reply with exactly the single word lattice. Do not use tools.";
const TURN_TWO: &str = "What single word did I just ask you to reply with? Answer with that word only. Do not use tools.";
const SIDE_PROMPT: &str = "What single word did we repeat in this chat? Reply with that word only.";
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

fn expect_words(first: &str, second: &str, other: &str) -> Result<(), String> {
    if first.trim() != WORD {
        return Err(format!(
            "first reply was not the expected word (len {})",
            first.trim().len()
        ));
    }
    if second.trim() != WORD {
        return Err(format!(
            "second reply lost context (len {})",
            second.trim().len()
        ));
    }
    if other.trim() != OTHER {
        return Err(format!(
            "second chat reply was not its own word (len {})",
            other.trim().len()
        ));
    }
    Ok(())
}

fn check_prompt_isolation() -> Result<(), String> {
    let leak = [TURN_TWO, SIDE_PROMPT, TURN_OTHER, OTHER]
        .iter()
        .any(|s| s.to_ascii_lowercase().contains(WORD));
    if leak {
        return Err("later prompts must not contain the first chat's word".into());
    }
    Ok(())
}

fn verify_reconnect_history(
    harness: &mut Harness,
    first_id: &str,
    second_id: &str,
) -> Result<(), String> {
    harness.send(Command::Reconnect)?;
    wait_connected(harness, Duration::from_secs(40))?;
    let cold_first = resume(harness, first_id)?;
    let cold_second = resume(harness, second_id)?;
    assert_context(&cold_first.1)?;
    assert_side_unpersisted(&cold_first.1)?;
    assert_separate(&cold_second.1, "cold resume of the second chat")
}

fn run(harness: &mut Harness, created: &mut Vec<String>) -> Result<String, String> {
    check_prompt_isolation()?;
    wait_connected(harness, Duration::from_secs(40))?;
    prove_conversations_and_timeline(harness, created)?;
    let switched = prove_model_switch(harness, created)?;
    Ok(format!(
        "connected created=4 turn1={WORD} turn2_context=true btw_snapshot=true branched_undo=true fts_search=true curation_pin_rename_archive=true second={OTHER} switched=true reconnect_history=true model_switch={switched}"
    ))
}

fn prove_conversations_and_timeline(
    harness: &mut Harness,
    created: &mut Vec<String>,
) -> Result<(), String> {
    let first = create(harness, "hn-smoke-a")?;
    created.push(first.0.clone());
    let second = create(harness, "hn-smoke-b")?;
    created.push(second.0.clone());
    prove_initial_turns(harness, &first.1, &second.1)?;
    prove_side_question(harness, &first.1)?;
    prove_branch_and_undo(harness, &first.0, &first.1, created)?;
    prove_saved_and_reconnected(harness, &first.0, &second.0)?;
    prove_recall_and_curation(harness, &first.0, &second.0)
}

fn prove_initial_turns(
    harness: &mut Harness,
    first_rt: &str,
    second_rt: &str,
) -> Result<(), String> {
    let first_reply = submit(harness, first_rt, TURN_ONE)?.0;
    let second_reply = submit(harness, first_rt, TURN_TWO)?.0;
    let other_reply = submit(harness, second_rt, TURN_OTHER)?.0;
    expect_words(&first_reply, &second_reply, &other_reply)
}

fn prove_saved_and_reconnected(
    harness: &mut Harness,
    first_id: &str,
    second_id: &str,
) -> Result<(), String> {
    let saved = resume(harness, second_id)?;
    assert_separate(&saved.1, "the second chat")?;
    let resumed = resume(harness, first_id)?;
    assert_context(&resumed.1)?;
    assert_side_unpersisted(&resumed.1)?;
    verify_reconnect_history(harness, first_id, second_id)
}

fn prove_recall_and_curation(
    harness: &mut Harness,
    first_id: &str,
    second_id: &str,
) -> Result<(), String> {
    curate_session(harness, first_id, SessionMutation::SetPinned(true))?;
    curate_session(
        harness,
        first_id,
        SessionMutation::Rename("hn-smoke-curated".into()),
    )?;
    verify_listed_curation(harness, first_id, "hn-smoke-curated")?;
    prove_fts_search(harness, first_id, WORD)?;
    curate_session(harness, second_id, SessionMutation::Archive)?;
    verify_archived_excluded(harness, second_id)
}

fn curate_session(
    harness: &mut Harness,
    stored_id: &str,
    mutation: SessionMutation,
) -> Result<(), String> {
    harness.send(Command::MutateSession {
        stored_id: stored_id.into(),
        mutation: mutation.clone(),
    })?;
    let start = Instant::now();
    let mut done = false;
    while start.elapsed() < Duration::from_secs(30) {
        for event in harness.drain() {
            if let Some(event) = note_mutation(event, stored_id, &mutation, &mut done)? {
                harness.events.push(event);
            }
        }
        if done {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err("session curation timed out".into())
}

fn note_mutation(
    event: Event,
    target_id: &str,
    expected: &SessionMutation,
    done: &mut bool,
) -> Result<Option<Event>, String> {
    match event {
        Event::SessionMutated {
            stored_id,
            mutation,
        } if stored_id == target_id && &mutation == expected => {
            *done = true;
            Ok(None)
        }
        Event::Error {
            operation,
            session_id,
            message,
        } if operation == "curation" && session_id.as_deref() == Some(target_id) => {
            Err(format!("curation failed: {message}"))
        }
        other => Ok(Some(other)),
    }
}

fn verify_listed_curation(
    harness: &mut Harness,
    target_id: &str,
    expected_title: &str,
) -> Result<(), String> {
    let sessions = fetch_first_page(harness)?;
    let row = sessions
        .iter()
        .find(|s| s.stored_id == target_id)
        .ok_or_else(|| "curated session missing from recent list".to_string())?;
    if !row.pinned || row.title != expected_title {
        return Err(format!(
            "curated session mismatch: pinned={} title={}",
            row.pinned, row.title
        ));
    }
    Ok(())
}

fn verify_archived_excluded(harness: &mut Harness, archived_id: &str) -> Result<(), String> {
    let sessions = fetch_first_page(harness)?;
    if sessions.iter().any(|s| s.stored_id == archived_id) {
        return Err("archived session still appeared in active session list".into());
    }
    Ok(())
}

fn fetch_first_page(harness: &mut Harness) -> Result<Vec<SessionSummary>, String> {
    harness.send(Command::List { offset: 0 })?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        for event in harness.drain() {
            match event {
                Event::Sessions {
                    sessions,
                    offset: 0,
                    ..
                } => return Ok(sessions),
                Event::Error {
                    operation, message, ..
                } if operation == "list" => return Err(format!("list failed: {message}")),
                other => harness.events.push(other),
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err("list timed out".into())
}

fn prove_fts_search(harness: &mut Harness, target_id: &str, query: &str) -> Result<(), String> {
    harness.send(Command::SearchSessions {
        query: query.into(),
        request_seq: 42,
    })?;
    let start = Instant::now();
    let mut hits = None;
    while start.elapsed() < Duration::from_secs(30) {
        for event in harness.drain() {
            if let Some(event) = note_search(event, 42, &mut hits)? {
                harness.events.push(event);
            }
        }
        if let Some(found) = hits {
            return verify_fts_hit(&found, target_id);
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err("FTS5 search timed out".into())
}

fn note_search(
    event: Event,
    expected_seq: u64,
    hits: &mut Option<Vec<SearchHit>>,
) -> Result<Option<Event>, String> {
    match event {
        Event::SearchResults {
            request_seq,
            hits: rows,
            ..
        } if request_seq == expected_seq => {
            *hits = Some(rows);
            Ok(None)
        }
        Event::Error {
            operation, message, ..
        } if operation == "search" => Err(format!("search failed: {message}")),
        other => Ok(Some(other)),
    }
}

fn verify_fts_hit(hits: &[SearchHit], target_id: &str) -> Result<(), String> {
    let hit = hits
        .iter()
        .find(|h| h.stored_id == target_id)
        .ok_or_else(|| "FTS5 search did not find target session".to_string())?;
    if !hit.pinned || hit.title != "hn-smoke-curated" {
        return Err("FTS5 hit did not reflect curated pin/title metadata".into());
    }
    if !hit.snippet_markup.contains("<b>") {
        return Err("FTS5 hit snippet did not include Pango bold highlight markup".into());
    }
    Ok(())
}

fn prove_branch_and_undo(
    harness: &mut Harness,
    parent_stored: &str,
    parent_runtime: &str,
    created: &mut Vec<String>,
) -> Result<(), String> {
    let (branch_stored, branch_runtime, branch_msgs) =
        branch_point_in_time(harness, parent_stored, parent_runtime)?;
    created.push(branch_stored.clone());
    if branch_msgs.len() != 2 {
        return Err(format!(
            "point-in-time branch expected 2 messages, got {}",
            branch_msgs.len()
        ));
    }
    let (branch_reply, _) = submit(harness, &branch_runtime, TURN_TWO)?;
    if branch_reply.trim() != WORD {
        return Err(format!(
            "branched chat lost parent context (len {})",
            branch_reply.trim().len()
        ));
    }
    let undone_msgs = undo_turn(harness, &branch_stored, &branch_runtime)?;
    if undone_msgs.len() != 2 {
        return Err(format!(
            "session.undo expected 2 remaining messages, got {}",
            undone_msgs.len()
        ));
    }
    Ok(())
}

fn branch_point_in_time(
    harness: &mut Harness,
    parent_stored: &str,
    parent_runtime: &str,
) -> Result<(String, String, Vec<ChatMessage>), String> {
    harness.send(Command::Branch {
        stored_id: parent_stored.into(),
        runtime_id: Some(parent_runtime.into()),
        parent_title: "hn-smoke-a".into(),
        before_loaded: 0,
        visible_count: Some(2),
        custom_title: Some("hn-smoke-branch".into()),
        seed_fallback: vec![
            SeedMessage {
                role: "user".into(),
                content: TURN_ONE.into(),
            },
            SeedMessage {
                role: "assistant".into(),
                content: WORD.into(),
            },
        ],
    })?;
    let start = Instant::now();
    let mut found = None;
    while start.elapsed() < Duration::from_secs(45) {
        for event in harness.drain() {
            if let Some(event) = note_branch(event, parent_stored, &mut found)? {
                harness.events.push(event);
            }
        }
        if let Some(out) = found {
            return Ok(out);
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err("session.branch timed out".into())
}

fn note_branch(
    event: Event,
    parent_stored: &str,
    found: &mut Option<(String, String, Vec<ChatMessage>)>,
) -> Result<Option<Event>, String> {
    match event {
        Event::Branched {
            session,
            runtime_id,
            messages,
            ..
        } if session.parent_session_id.as_deref() == Some(parent_stored) => {
            *found = Some((session.stored_id, runtime_id, messages));
            Ok(None)
        }
        Event::Error {
            operation, message, ..
        } if operation == "branch" => Err(format!("branch failed: {message}")),
        Event::Connection(ConnectionState::Disconnected(message)) => {
            Err(format!("disconnected during branch: {message}"))
        }
        other => Ok(Some(other)),
    }
}

fn undo_turn(
    harness: &mut Harness,
    stored_id: &str,
    runtime_id: &str,
) -> Result<Vec<ChatMessage>, String> {
    harness.send(Command::Undo {
        stored_id: stored_id.into(),
        runtime_id: runtime_id.into(),
        mode: RewindMode::Undo,
        prompt_text: TURN_TWO.into(),
    })?;
    let start = Instant::now();
    let mut undone = None;
    while start.elapsed() < Duration::from_secs(45) {
        for event in harness.drain() {
            if let Some(event) = note_undo(event, stored_id, &mut undone)? {
                harness.events.push(event);
            }
        }
        if let Some(messages) = undone {
            return Ok(messages);
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err("session.undo timed out".into())
}

fn note_undo(
    event: Event,
    target_stored: &str,
    undone: &mut Option<Vec<ChatMessage>>,
) -> Result<Option<Event>, String> {
    match event {
        Event::Undone {
            stored_id,
            removed,
            messages,
            ..
        } if stored_id == target_stored => {
            if removed < 2 {
                return Err(format!(
                    "session.undo removed {removed} rows, expected >= 2"
                ));
            }
            *undone = Some(messages);
            Ok(None)
        }
        Event::Error {
            operation,
            session_id,
            message,
        } if operation == "undo" && session_id.as_deref() == Some(target_stored) => {
            Err(format!("undo failed: {message}"))
        }
        Event::Connection(ConnectionState::Disconnected(message)) => {
            Err(format!("disconnected during undo: {message}"))
        }
        other => Ok(Some(other)),
    }
}

fn note_side(
    event: Event,
    runtime_id: &str,
    answer: &mut Option<String>,
) -> Result<Option<Event>, String> {
    match event {
        Event::SideStarted { runtime_id: id, .. } if id == runtime_id => Ok(None),
        Event::SideComplete {
            runtime_id: id,
            text,
            ..
        } if id == runtime_id => {
            *answer = Some(text);
            Ok(None)
        }
        Event::SideFailed {
            runtime_id: id,
            message,
            ..
        } if id == runtime_id => Err(format!("btw side question failed: {message}")),
        Event::Connection(ConnectionState::Disconnected(message)) => {
            Err(format!("disconnected during btw: {message}"))
        }
        event => Ok(Some(event)),
    }
}

fn prove_side_question(harness: &mut Harness, runtime_id: &str) -> Result<(), String> {
    harness.send(Command::AskSide {
        runtime_id: runtime_id.into(),
        client_id: 1,
        question: SIDE_PROMPT.into(),
    })?;
    let start = Instant::now();
    let mut answer = None;
    while start.elapsed() < Duration::from_secs(90) {
        for event in harness.drain() {
            if let Some(event) = note_side(event, runtime_id, &mut answer)? {
                harness.events.push(event);
            }
        }
        if let Some(text) = answer {
            return verify_side_word(&text);
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err("btw side question timed out".into())
}

fn verify_side_word(text: &str) -> Result<(), String> {
    if text.to_ascii_lowercase().contains(WORD) {
        Ok(())
    } else {
        Err(format!(
            "btw side answer missed session context (len {})",
            text.trim().len()
        ))
    }
}

fn assert_side_unpersisted(messages: &[ChatMessage]) -> Result<(), String> {
    if messages.iter().any(|m| m.text.contains(SIDE_PROMPT)) {
        return Err("ephemeral btw question leaked into persisted history".into());
    }
    Ok(())
}

fn confirm_switch(
    reply: &str,
    used: Option<&str>,
    target: &str,
    after: Option<&ModelIdentity>,
    default: &ModelIdentity,
) -> Result<(), String> {
    if reply.trim() != OTHER {
        return Err("the switched chat lost its own word".into());
    }
    if used != Some(target) {
        return Err(format!(
            "the first inference did not use the chosen model (reported {used:?})"
        ));
    }
    if after != Some(default) {
        return Err("the switch changed the server default".into());
    }
    Ok(())
}

fn prove_model_switch(harness: &mut Harness, created: &mut Vec<String>) -> Result<String, String> {
    let before = catalog(harness)?;
    let default = before
        .default
        .clone()
        .ok_or("the catalog did not report a default model")?;
    let target = switch_target(&before, &default)?;
    let switched = create(harness, "hn-smoke-model")?;
    created.push(switched.0);
    switch_model(harness, &switched.1, Some(default.clone()), &target)?;
    let (reply, used) = submit(harness, &switched.1, TURN_OTHER)?;
    let after = catalog(harness)?;
    confirm_switch(
        &reply,
        used.as_deref(),
        &target.model,
        after.default.as_ref(),
        &default,
    )?;
    Ok(target.model)
}

fn switch_target(before: &Catalog, default: &ModelIdentity) -> Result<ModelIdentity, String> {
    before
        .options
        .iter()
        .find(|option| {
            option.selectable()
                && option.identity.provider == default.provider
                && option.identity != *default
        })
        .or_else(|| {
            before.options.iter().find(|option| {
                option.selectable()
                    && option.identity != *default
                    && option.identity.provider != default.provider
            })
        })
        .map(|option| option.identity.clone())
        .ok_or_else(|| "the catalog needs a second selectable model to prove switching".into())
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
                    // A chat that never ran persists no row, so there is nothing left to delete.
                    return if message.to_ascii_lowercase().contains("session not found") {
                        Ok(())
                    } else {
                        Err(message)
                    };
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
                    model: _,
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

fn catalog(harness: &mut Harness) -> Result<Catalog, String> {
    harness.send(Command::FetchModels)?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(45) {
        for event in harness.drain() {
            match event {
                Event::Catalog { catalog } => {
                    if let Some(error) = catalog.error {
                        return Err(error);
                    }
                    if catalog.is_empty() {
                        return Err("the server has no configured models".into());
                    }
                    return Ok(catalog);
                }
                event => harness.events.push(event),
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err("catalog timed out".into())
}

fn switch_model(
    harness: &mut Harness,
    runtime_id: &str,
    prime: Option<ModelIdentity>,
    target: &ModelIdentity,
) -> Result<(), String> {
    harness.send(Command::SetModel {
        runtime_id: runtime_id.into(),
        prime,
        target: target.clone(),
        confirm_expensive: false,
    })?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(90) {
        for event in harness.drain() {
            match event {
                Event::ModelAck { runtime_id: id, .. } if id == runtime_id => return Ok(()),
                Event::ModelRejected {
                    runtime_id: id,
                    message,
                } if id == runtime_id => return Err(message),
                Event::ModelConfirmation {
                    runtime_id: id,
                    message,
                    ..
                } if id == runtime_id => {
                    return Err(format!("the switch needed a confirmation: {message}"));
                }
                Event::ModelAmbiguous { runtime_id: id } if id == runtime_id => {
                    return Err("the switch outcome was never reported".into());
                }
                event => harness.events.push(event),
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err("model switch timed out".into())
}

/// Submit a prompt and return its reply text plus the model the completion reported.
fn note_submit(
    event: Event,
    runtime_id: &str,
    reply: &mut Option<String>,
    observed: &mut Option<String>,
) -> Result<Option<Event>, String> {
    match event {
        Event::ModelObserved {
            runtime_id: id,
            model,
        } if id == runtime_id => {
            *observed = Some(model);
            Ok(None)
        }
        Event::MessageComplete {
            runtime_id: id,
            message,
            status,
        } if id == runtime_id => {
            if status != TurnStatus::Complete {
                return Err(format!("turn ended as {status:?}"));
            }
            *reply = Some(message.text);
            Ok(None)
        }
        Event::Error {
            operation,
            session_id,
            message,
        } if session_id.as_deref() == Some(runtime_id)
            && matches!(operation.as_str(), "submit" | "gateway") =>
        {
            Err(message)
        }
        Event::Connection(ConnectionState::Disconnected(message)) => {
            Err(format!("disconnected during turn: {message}"))
        }
        event => Ok(Some(event)),
    }
}

fn submit(
    harness: &mut Harness,
    runtime_id: &str,
    text: &str,
) -> Result<(String, Option<String>), String> {
    harness.send(Command::Submit {
        runtime_id: runtime_id.into(),
        text: text.into(),
    })?;
    let start = Instant::now();
    let mut reply = None;
    let mut observed = None;
    while start.elapsed() < Duration::from_secs(180) {
        for event in harness.drain() {
            if let Some(event) = note_submit(event, runtime_id, &mut reply, &mut observed)? {
                harness.events.push(event);
            }
        }
        if let Some(reply) = reply.take() {
            return Ok((reply, await_observed(harness, runtime_id, observed)));
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err("turn timed out; the prompt was not sent again".into())
}

fn await_observed(
    harness: &mut Harness,
    runtime_id: &str,
    mut observed: Option<String>,
) -> Option<String> {
    let settle = Instant::now();
    while observed.is_none() && settle.elapsed() < Duration::from_secs(5) {
        for event in harness.drain() {
            match event {
                Event::ModelObserved {
                    runtime_id: id,
                    model,
                } if id == runtime_id => {
                    observed = Some(model);
                }
                event => harness.events.push(event),
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    observed
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
