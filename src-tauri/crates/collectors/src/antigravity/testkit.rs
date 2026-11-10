//! Synthetic Antigravity conversation databases for tests. Tables, columns and
//! protobuf field numbers mirror those verified on IDE 2.5.5 and app 2.0.11
//! (docs/providers/antigravity.md). Every content-bearing column and field carries a
//! sentinel, so privacy tests can prove it never leaves the parser.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use rusqlite::{Connection, params};

pub use super::proto::encode::Msg;

/// Model enum numbers seen in real data, with their `response_model` names.
pub const GEMINI_PRO: u64 = 1016;
pub const CLAUDE_OPUS: u64 = 1026;
/// Only ever seen on auxiliary steps, with no generation record (so no name).
pub const AUX_MODEL: u64 = 1050;

#[derive(Clone, Copy, Default)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub thinking: u64,
}

#[derive(Clone)]
struct StepRow {
    step_type: i64,
    metadata: Vec<u8>,
    step_format: i64,
}

/// One conversation database.
pub struct Conversation {
    sentinel: String,
    steps: Vec<StepRow>,
    /// (index of the step it belongs to, record).
    gens: Vec<(usize, Vec<u8>)>,
}

fn usage(model: u64, t: Tokens, response_id: Option<&str>, sentinel: &str) -> Msg {
    let mut u = Msg::new().varint(1, model).varint(2, t.input).varint(3, t.output).varint(6, 3);
    for (field, v) in [(4, t.cache_write), (5, t.cache_read), (9, t.thinking), (10, t.output - t.thinking)] {
        // Zero-valued fields are omitted on the wire, as in the real data.
        if v > 0 {
            u = u.varint(field, v);
        }
    }
    if let Some(r) = response_id {
        u = u.str(11, r);
    }
    u.str(7, &format!("msg-{sentinel}")).str(12, &format!("prov-{sentinel}"))
}

fn timestamp(secs: i64) -> Msg {
    Msg::new().varint(1, secs as u64).varint(2, 500_000_000)
}

impl Conversation {
    pub fn new(sentinel: &str) -> Self {
        Self { sentinel: sentinel.to_owned(), steps: Vec::new(), gens: Vec::new() }
    }

    /// A model call of the conversation itself: a step (type 15) plus its
    /// generation record, which repeats the same usage and names the model.
    pub fn call(&mut self, response_id: &str, model: u64, name: Option<&str>, secs: i64, t: Tokens) -> &mut Self {
        let s = self.sentinel.clone();
        let u = usage(model, t, Some(response_id), &s);
        let meta = Msg::new().msg(1, &timestamp(secs)).msg(9, &u).varint(11, model).varint(14, 2).str(99, &format!("meta-{s}"));
        self.steps.push(StepRow { step_type: 15, metadata: meta.0, step_format: 0 });
        let mut chat = Msg::new()
            .str(1, &format!("system prompt {s}"))
            .msg(2, &Msg::new().str(1, &format!("user prompt {s}")))
            .varint(3, model)
            .msg(4, &u)
            .varint(13, 1)
            .msg(17, &Msg::new().msg(2, &u).str(3, &format!("error {s}")));
        if let Some(n) = name {
            chat = chat.str(19, n).str(21, &format!("{n} (Display)"));
        }
        let idx = self.steps.len() - 1;
        let generator = Msg::new().msg(1, &chat).varint(2, idx as u64).str(4, &format!("exec-{s}")).str(9, &format!("debug {s}"));
        self.gens.push((idx, generator.0));
        self
    }

    /// A generation record that names `model` by display name only (no API id), as
    /// some real records do.
    pub fn display_only_record(&mut self, model: u64, display: &str) -> &mut Self {
        let chat = Msg::new().str(1, &format!("system prompt {}", self.sentinel)).varint(3, model).str(21, display);
        let idx = self.steps.len().saturating_sub(1);
        self.gens.push((idx, Msg::new().msg(1, &chat).0));
        self
    }

    /// An auxiliary call (type 23): usage, but no generator model and no generation record.
    pub fn aux_call(&mut self, response_id: &str, secs: i64, t: Tokens) -> &mut Self {
        let u = usage(AUX_MODEL, t, Some(response_id), &self.sentinel);
        let meta = Msg::new().msg(1, &timestamp(secs)).msg(9, &u);
        self.steps.push(StepRow { step_type: 23, metadata: meta.0, step_format: 0 });
        self
    }

    /// A step without usage (a user turn, a tool run).
    pub fn plain_step(&mut self, secs: i64) -> &mut Self {
        let meta = Msg::new().msg(1, &timestamp(secs)).str(5, &format!("tool {}", self.sentinel));
        self.steps.push(StepRow { step_type: 5, metadata: meta.0, step_format: 0 });
        self
    }

    /// A call in a `step_format` AgentPulse doesn't know.
    pub fn future_format_call(&mut self, response_id: &str, secs: i64, t: Tokens) -> &mut Self {
        let u = usage(GEMINI_PRO, t, Some(response_id), &self.sentinel);
        let meta = Msg::new().msg(1, &timestamp(secs)).msg(9, &u).varint(11, GEMINI_PRO);
        self.steps.push(StepRow { step_type: 15, metadata: meta.0, step_format: 7 });
        self
    }

    /// A step whose metadata isn't protobuf.
    pub fn garbage_step(&mut self) -> &mut Self {
        self.steps.push(StepRow { step_type: 15, metadata: vec![0xff, 0xff, 0x01], step_format: 0 });
        self
    }

    fn fill(&self, c: &Connection, from: usize) {
        let s = &self.sentinel;
        if from == 0 {
            c.execute_batch(
                "CREATE TABLE trajectory_meta(trajectory_id TEXT PRIMARY KEY, cascade_id TEXT, trajectory_type INTEGER, source INTEGER);
                 CREATE TABLE steps(idx INTEGER PRIMARY KEY, step_type INTEGER, status INTEGER, has_subtrajectory INTEGER,
                     metadata BLOB, error_details BLOB, permissions BLOB, task_details BLOB, render_info BLOB,
                     step_payload BLOB, step_format INTEGER);
                 CREATE TABLE gen_metadata(idx INTEGER PRIMARY KEY, data BLOB, size INTEGER);
                 CREATE TABLE executor_metadata(idx INTEGER PRIMARY KEY, data BLOB);
                 CREATE TABLE trajectory_metadata_blob(id INTEGER PRIMARY KEY, data BLOB);
                 CREATE TABLE battle_mode_infos(idx INTEGER PRIMARY KEY, data BLOB);
                 CREATE TABLE parent_references(idx INTEGER PRIMARY KEY, data BLOB);",
            )
            .unwrap();
            c.execute("INSERT INTO trajectory_meta VALUES (?1, ?2, 1, 1)", params![format!("traj-{s}"), format!("cascade-{s}")])
                .unwrap();
            c.execute("INSERT INTO executor_metadata VALUES (0, ?1)", [format!("exec {s}").into_bytes()]).unwrap();
            c.execute("INSERT INTO trajectory_metadata_blob VALUES (0, ?1)", [format!("blob {s}").into_bytes()]).unwrap();
        }
        for (i, row) in self.steps.iter().enumerate().skip(from) {
            c.execute(
                "INSERT INTO steps VALUES (?1, ?2, 3, 0, ?3, ?4, NULL, ?5, NULL, ?6, ?7)",
                params![
                    i as i64,
                    row.step_type,
                    row.metadata,
                    format!("error {s}").into_bytes(),
                    format!("task {s}").into_bytes(),
                    format!("payload: prompt and code {s}").into_bytes(),
                    row.step_format
                ],
            )
            .unwrap();
        }
        for (i, (_, g)) in self.gens.iter().enumerate().filter(|(_, (step, _))| *step >= from) {
            c.execute("INSERT INTO gen_metadata VALUES (?1, ?2, ?3)", params![i as i64, g, g.len() as i64]).unwrap();
        }
    }

    /// Write a database with no pending log (as after a clean shutdown).
    pub fn write(&self, path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(path);
        let c = Connection::open(path).unwrap();
        self.fill(&c, 0);
    }

    /// Write a WAL-mode database whose last `pending` steps exist only in
    /// `<path>-wal`, not yet checkpointed (as while Antigravity is running).
    pub fn write_with_pending_wal(&self, path: &Path, pending: usize) {
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir).unwrap();
        let work = dir.join(format!(".work-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).unwrap();
        let tmp = work.join("t.db");
        let c = Connection::open(&tmp).unwrap();
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;").unwrap();
        let split = self.steps.len().saturating_sub(pending);
        let head = Conversation {
            sentinel: self.sentinel.clone(),
            steps: self.steps[..split].to_vec(),
            gens: self.gens.iter().filter(|(step, _)| *step < split).cloned().collect(),
        };
        head.fill(&c, 0);
        c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        self.fill(&c, split);
        // Copy while the connection is open, so the log isn't checkpointed away.
        std::fs::copy(&tmp, path).unwrap();
        std::fs::copy(work.join("t.db-wal"), super::files::wal_path(path)).unwrap();
        drop(c);
        std::fs::remove_dir_all(&work).unwrap();
    }
}

/// Write `<data folder>/conversations/<id>.db`; returns its path.
pub fn write_conversation(data_folder: &Path, id: &str, c: &Conversation) -> std::path::PathBuf {
    let path = data_folder.join("conversations").join(format!("{id}.db"));
    c.write(&path);
    path
}

/// Files Antigravity keeps that AgentPulse must never open, each holding `sentinel`.
/// The backup folder even holds a real usage database: it must not be counted.
pub fn forbidden_files(gemini_dir: &Path, data_folder: &str, sentinel: &str) {
    let root = gemini_dir.join(data_folder);
    let conv = root.join("conversations");
    std::fs::create_dir_all(&conv).unwrap();
    std::fs::write(conv.join(format!("old-{sentinel}.pb")), format!("encrypted {sentinel}")).unwrap();
    std::fs::write(conv.join("stale.db-shm"), sentinel).unwrap();
    for (dir, file) in [("brain", "plan.md"), ("knowledge", "k.json"), ("daemon", "ls_1.json")] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
        std::fs::write(root.join(dir).join(file), format!("{{\"csrfToken\":\"{sentinel}\",\"text\":\"{sentinel}\"}}")).unwrap();
    }
    std::fs::write(gemini_dir.join("oauth_creds.json"), format!("{{\"access_token\":\"{sentinel}\"}}")).unwrap();
    let mut backup = Conversation::new(sentinel);
    backup.call("backup-call", GEMINI_PRO, Some(sentinel), 1_762_000_000, Tokens { input: 1, output: 1, ..Tokens::default() });
    write_conversation(&gemini_dir.join("antigravity-backup"), "backup", &backup);
}
