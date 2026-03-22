use rusqlite::{params, Connection, Result as SqlResult};
use std::sync::{Arc, Mutex};

pub type Db = Arc<Mutex<Connection>>;

pub fn open(path: &str) -> SqlResult<Db> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "
        PRAGMA journal_mode=WAL;

        CREATE TABLE IF NOT EXISTS devices (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            display_id  TEXT    NOT NULL UNIQUE,  -- e.g. '847-291-035'
            name        TEXT    NOT NULL,
            os          TEXT    NOT NULL DEFAULT 'Unknown',
            status      TEXT    NOT NULL DEFAULT 'offline',
            serial_ports TEXT   NOT NULL DEFAULT '[]',  -- JSON array
            last_seen   INTEGER NOT NULL DEFAULT 0      -- unix ms
        );

        CREATE TABLE IF NOT EXISTS sessions (
            device_id   TEXT    NOT NULL,
            session_key TEXT    NOT NULL UNIQUE,
            created_at  INTEGER NOT NULL
        );
    ",
    )?;
    Ok(Arc::new(Mutex::new(conn)))
}

/// Format a raw integer autoincrement ID into AnyDesk-style display string
pub fn format_display_id(raw: i64) -> String {
    // e.g. 847291035 → "847-291-035"
    let s = format!("{:09}", raw);
    format!("{}-{}-{}", &s[0..3], &s[3..6], &s[6..9])
}

pub fn claim_id(db: &Db, name: &str, os: &str) -> SqlResult<String> {
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT INTO devices (display_id, name, os) VALUES ('PENDING', ?, ?)",
        params![name, os],
    )?;
    let raw_id = conn.last_insert_rowid();
    let display = format_display_id(raw_id);
    conn.execute(
        "UPDATE devices SET display_id = ? WHERE id = ?",
        params![display, raw_id],
    )?;
    Ok(display)
}

pub fn mark_online(db: &Db, display_id: &str, serial_ports_json: &str) -> SqlResult<bool> {
    let conn = db.lock().unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let rows = conn.execute(
        "UPDATE devices SET status='online', serial_ports=?, last_seen=? WHERE display_id=?",
        params![serial_ports_json, now, display_id],
    )?;
    Ok(rows > 0)
}

pub fn mark_offline(db: &Db, display_id: &str) -> SqlResult<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE devices SET status='offline' WHERE display_id=?",
        params![display_id],
    )?;
    Ok(())
}

pub fn lookup(db: &Db, display_id: &str) -> SqlResult<Option<DeviceRow>> {
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT display_id, name, os, status, serial_ports, last_seen FROM devices WHERE display_id=?"
    )?;
    let mut rows = stmt.query_map(params![display_id], |row| {
        Ok(DeviceRow {
            display_id: row.get(0)?,
            name: row.get(1)?,
            os: row.get(2)?,
            status: row.get(3)?,
            serial_ports: row.get(4)?,
            last_seen: row.get(5)?,
        })
    })?;
    Ok(rows.next().transpose()?)
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct DeviceRow {
    pub display_id: String,
    pub name: String,
    pub os: String,
    pub status: String,
    pub serial_ports: String, // JSON string
    pub last_seen: i64,
}
