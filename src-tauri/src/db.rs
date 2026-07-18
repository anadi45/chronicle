use rusqlite::{Connection, Result};

pub struct Database {
    connection: Connection,
}

impl Database {
    pub fn open() -> Result<Self> {
        let mut connection = Connection::open("chronicle.db")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.execute_batch(include_str!("../migrations/001_initial.sql"))?;
        Ok(Self { connection })
    }

    pub fn count_events(&self) -> Result<i64> {
        self.connection
            .query_row("SELECT COUNT(*) FROM raw_events", [], |row| row.get(0))
    }
}
