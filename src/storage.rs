use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use directories::BaseDirs;
use fs2::FileExt;

use crate::error::{LokinError, Result};
use crate::session::{SCHEMA_VERSION, StoredEvent};

const DATA_DIR_ENV: &str = "LOKIN_DATA_DIR";
const EVENTS_FILE: &str = "events.jsonl";
const LOCK_FILE: &str = "events.lock";

#[derive(Clone, Debug)]
pub struct EventStore {
    data_dir: PathBuf,
    events_path: PathBuf,
    lock_path: PathBuf,
}

impl EventStore {
    pub fn from_environment() -> Result<Self> {
        let data_dir = match env::var_os(DATA_DIR_ENV) {
            Some(path) if !path.is_empty() => PathBuf::from(path),
            _ => BaseDirs::new()
                .map(|directories| directories.data_local_dir().join("lokin"))
                .ok_or(LokinError::DataDirectoryUnavailable)?,
        };
        Ok(Self::new(data_dir))
    }

    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            events_path: data_dir.join(EVENTS_FILE),
            lock_path: data_dir.join(LOCK_FILE),
            data_dir,
        }
    }

    #[cfg(test)]
    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    pub fn read_all(&self) -> Result<Vec<StoredEvent>> {
        let lock = self.open_lock()?;
        FileExt::lock_shared(&lock)?;
        let result = self.read_unlocked();
        let unlock_result = FileExt::unlock(&lock);
        result.and_then(|events| {
            unlock_result?;
            Ok(events)
        })
    }

    pub fn mutate<T, F>(&self, operation: F) -> Result<T>
    where
        F: FnOnce(&[StoredEvent]) -> Result<(T, Vec<StoredEvent>)>,
    {
        let lock = self.open_lock()?;
        FileExt::lock_exclusive(&lock)?;
        let result = (|| {
            let current = self.read_unlocked()?;
            let (value, new_events) = operation(&current)?;
            self.append_unlocked(&new_events)?;
            Ok(value)
        })();
        let unlock_result = FileExt::unlock(&lock);
        result.and_then(|value| {
            unlock_result?;
            Ok(value)
        })
    }

    fn open_lock(&self) -> Result<File> {
        fs::create_dir_all(&self.data_dir)?;
        Ok(OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.lock_path)?)
    }

    fn read_unlocked(&self) -> Result<Vec<StoredEvent>> {
        let file = match File::open(&self.events_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut events = Vec::new();

        for (index, line) in BufReader::new(file).lines().enumerate() {
            let line_number = index + 1;
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let event: StoredEvent = serde_json::from_str(&line).map_err(|error| {
                LokinError::CorruptLog(format!("line {line_number} is not valid JSON: {error}"))
            })?;
            if event.schema_version != SCHEMA_VERSION {
                return Err(LokinError::UnsupportedSchemaVersion(event.schema_version));
            }
            events.push(event);
        }
        Ok(events)
    }

    fn append_unlocked(&self, events: &[StoredEvent]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }

        let mut payload = Vec::new();
        for event in events {
            serde_json::to_writer(&mut payload, event)?;
            payload.push(b'\n');
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events_path)?;
        file.write_all(&payload)?;
        file.flush()?;
        file.sync_data()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use tempfile::tempdir;
    use uuid::Uuid;

    use crate::session::SessionEvent;

    use super::*;

    fn start_event() -> StoredEvent {
        StoredEvent::new(
            Uuid::new_v4(),
            Utc::now(),
            SessionEvent::SessionStarted {
                goal: "Test persistence".to_string(),
                duration_minutes: 25,
            },
        )
    }

    #[test]
    fn creates_directory_and_round_trips_events() {
        let directory = tempdir().unwrap();
        let store = EventStore::new(directory.path().join("nested"));
        let event = start_event();

        store
            .mutate(|events| {
                assert!(events.is_empty());
                Ok(((), vec![event.clone()]))
            })
            .unwrap();

        assert_eq!(store.read_all().unwrap(), vec![event]);
    }

    #[test]
    fn appends_batches_in_order() {
        let directory = tempdir().unwrap();
        let store = EventStore::new(directory.path().to_path_buf());
        let started = start_event();
        let paused = StoredEvent::new(
            started.session_id,
            started.occurred_at,
            SessionEvent::SessionPaused,
        );

        store
            .mutate(|_| Ok(((), vec![started.clone(), paused.clone()])))
            .unwrap();

        assert_eq!(store.read_all().unwrap(), vec![started, paused]);
    }

    #[test]
    fn failed_mutation_appends_nothing() {
        let directory = tempdir().unwrap();
        let store = EventStore::new(directory.path().to_path_buf());

        let result: Result<()> = store.mutate(|_| {
            Err(LokinError::InvalidTransition(
                "transition rejected".to_string(),
            ))
        });

        assert!(matches!(result, Err(LokinError::InvalidTransition(_))));
        assert!(store.read_all().unwrap().is_empty());
    }

    #[test]
    fn reports_corrupt_line_number() {
        let directory = tempdir().unwrap();
        let store = EventStore::new(directory.path().to_path_buf());
        fs::create_dir_all(store.data_dir()).unwrap();
        fs::write(&store.events_path, "\nnot-json\n").unwrap();

        let error = store.read_all().unwrap_err();
        assert!(error.to_string().contains("line 2"));
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let directory = tempdir().unwrap();
        let store = EventStore::new(directory.path().to_path_buf());
        fs::create_dir_all(store.data_dir()).unwrap();
        let event = start_event();
        let line = serde_json::to_string(&event)
            .unwrap()
            .replace("\"schema_version\":1", "\"schema_version\":2");
        fs::write(&store.events_path, format!("{line}\n")).unwrap();

        assert!(matches!(
            store.read_all(),
            Err(LokinError::UnsupportedSchemaVersion(2))
        ));
    }
}
