use super::*;
use crate::native::Row;
use rusqlite::OptionalExtension;
use std::{collections::BTreeSet, io::Read};

#[derive(Default)]
pub(crate) struct CaptureUpdates {
    reload: bool,
    rows: BTreeMap<String, Row>,
}

pub struct CaptureBatch {
    pub group: String,
    pub replace: bool,
    pub rows: BTreeMap<String, Row>,
}

pub struct HistoryDay {
    pub date: String,
    pub total: usize,
}

pub struct HistoryPage {
    pub rows: Vec<Row>,
    pub days: Vec<HistoryDay>,
}

pub enum HistoryDeletion {
    Selected(Vec<String>),
    Dates(Vec<String>),
}

pub struct HistoryDeletePlan {
    pub dates: Vec<String>,
    pub active: usize,
    ids: Vec<String>,
}

impl HistoryDeletePlan {
    pub fn count(&self) -> usize {
        self.ids.len()
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LegacySpace {
    cleared_through: String,
    hidden: BTreeSet<String>,
}

const GROUP: &str = "COALESCE(NULLIF(json_extract(document, '$.captureGroup'), ''), 'default')";
const HISTORY_DATE: &str = "COALESCE(history_ids.day, date(sessions.started_at, 'localtime'))";

impl History {
    pub(super) fn initialize_history_ids(db: &Connection) -> Result<()> {
        db.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS history_sequences (day TEXT PRIMARY KEY, last_id INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS history_ids (
                 session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                 day TEXT NOT NULL,
                 number INTEGER NOT NULL,
                 UNIQUE(day, number)
             );
             CREATE TRIGGER IF NOT EXISTS assign_history_id AFTER INSERT ON sessions
             WHEN date(NEW.started_at, 'localtime') IS NOT NULL
             BEGIN
                 INSERT INTO history_sequences (day, last_id)
                     VALUES (date(NEW.started_at, 'localtime'), 1)
                     ON CONFLICT(day) DO UPDATE SET last_id = last_id + 1;
                 INSERT INTO history_ids (session_id, day, number)
                     SELECT NEW.id, day, last_id FROM history_sequences
                     WHERE day = date(NEW.started_at, 'localtime');
             END;",
        )?;
        Ok(())
    }

    pub(crate) fn enable_desktop_captures(&mut self) -> Result<()> {
        let has_archive = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('sessions') WHERE name = 'archived')",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        if !has_archive {
            let spaces = fs::File::open(self.directory.join("desktop-captures.json"))
                .ok()
                .and_then(|file| {
                    serde_json::from_reader::<_, BTreeMap<String, LegacySpace>>(
                        file.take(4 * 1024 * 1024 + 1),
                    )
                    .ok()
                });
            let transaction = self.db.transaction()?;
            transaction.execute(
                "ALTER TABLE sessions ADD COLUMN archived INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
            if let Some(spaces) = spaces {
                let groups: Vec<String> = transaction
                    .prepare(&format!("SELECT DISTINCT {GROUP} FROM sessions"))?
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                for group in groups {
                    if let Some(space) = spaces.get(&group) {
                        transaction.execute(
                            &format!("UPDATE sessions SET archived = 1 WHERE {GROUP} = ? AND started_at <= ?"),
                            params![group, space.cleared_through],
                        )?;
                        for id in &space.hidden {
                            transaction.execute(
                                &format!(
                                    "UPDATE sessions SET archived = 1 WHERE id = ? AND {GROUP} = ?"
                                ),
                                params![id, group],
                            )?;
                        }
                    } else if group != "default" && group != self.capture_group {
                        transaction.execute(
                            &format!("UPDATE sessions SET archived = 1 WHERE {GROUP} = ?"),
                            [&group],
                        )?;
                    }
                }
            }
            transaction.commit()?;
        }
        self.db.execute(
            &format!("CREATE INDEX IF NOT EXISTS sessions_active_group ON sessions({GROUP}, started_at) WHERE archived = 0"),
            [],
        )?;
        self.desktop_updates = Some(CaptureUpdates {
            reload: true,
            ..Default::default()
        });
        Ok(())
    }

    pub(super) fn is_archived(&self, id: &str) -> Result<Option<bool>> {
        Ok(self
            .db
            .query_row("SELECT archived FROM sessions WHERE id = ?", [id], |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub(super) fn publish_capture(&mut self, session: &Session) {
        if let Some(updates) = &mut self.desktop_updates {
            updates.rows.insert(session.id.clone(), Row::from(session));
        }
    }

    pub(super) fn reset_capture_updates(&mut self) {
        if let Some(updates) = &mut self.desktop_updates {
            updates.rows.clear();
            updates.reload = true;
        }
    }

    pub(crate) fn take_capture_batch(
        &mut self,
        group: &str,
        switched: bool,
    ) -> Result<Option<CaptureBatch>> {
        let Some(updates) = &mut self.desktop_updates else {
            return Ok(None);
        };
        let replace = switched || updates.reload;
        let rows = if replace {
            let mut statement = self.db.prepare(&format!(
                "SELECT document FROM sessions WHERE archived = 0 AND {GROUP} = ?"
            ))?;
            let mut result = statement.query([group])?;
            let mut rows = BTreeMap::new();
            while let Some(row) = result.next()? {
                let document = row.get_ref(0)?.as_str().map_err(crate::wire::Error::bad)?;
                let row = Row::from(&serde_json::from_str::<Session>(document)?);
                rows.insert(row.id.clone(), row);
            }
            rows.extend(
                self.sessions
                    .iter()
                    .filter(|s| s.incognito && s.capture_group == group)
                    .map(|session| (session.id.clone(), Row::from(session))),
            );
            updates.rows.clear();
            updates.reload = false;
            rows
        } else {
            let mut rows = std::mem::take(&mut updates.rows);
            rows.retain(|_, row| row.group == group);
            if rows.is_empty() {
                return Ok(None);
            }
            rows
        };
        Ok(Some(CaptureBatch {
            group: group.into(),
            replace,
            rows,
        }))
    }

    pub(crate) fn archive_captures(
        &mut self,
        group: &str,
        ids: Option<&[String]>,
    ) -> Result<(usize, usize)> {
        let transaction = self.db.transaction()?;
        let mut count = 0;
        if let Some(ids) = ids {
            let mut update = transaction.prepare(&format!(
                "UPDATE sessions SET archived = 1 WHERE archived = 0 AND {GROUP} = ? AND id = ?"
            ))?;
            for id in ids {
                count += update.execute(params![group, id])?;
            }
        } else {
            count = transaction.execute(
                &format!("UPDATE sessions SET archived = 1 WHERE archived = 0 AND {GROUP} = ?"),
                [group],
            )?;
        }
        transaction.commit()?;
        let discarded = self
            .sessions
            .iter()
            .filter(|row| {
                row.incognito
                    && row.capture_group == group
                    && ids.is_none_or(|ids| ids.contains(&row.id))
            })
            .count();
        if let Some(ids) = ids {
            self.sessions.retain(|row| {
                let keep = row.capture_group != group || !ids.contains(&row.id);
                if !keep {
                    self.session_revisions.remove(&row.id);
                }
                keep
            });
            self.volatile_bodies
                .retain(|(id, _), _| self.session_revisions.contains_key(id));
            self.revision += 1;
            self.reset_capture_updates();
        } else {
            self.clear_group(group);
        }
        Ok((count, discarded))
    }

    pub(crate) fn desktop_history_page(
        &self,
        date: Option<&str>,
        offset: usize,
        query: &str,
    ) -> Result<HistoryPage> {
        let days = self
            .db
            .prepare(&format!(
                "SELECT {HISTORY_DATE} AS history_date, count(*) FROM sessions
                 LEFT JOIN history_ids ON sessions.id = history_ids.session_id
                 WHERE {HISTORY_DATE} IS NOT NULL GROUP BY history_date ORDER BY history_date DESC",
            ))?
            .query_map([], |row| {
                Ok(HistoryDay {
                    date: row.get(0)?,
                    total: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut statement = self.db.prepare(&format!(
            "SELECT document, {HISTORY_DATE}, history_ids.number FROM sessions
             LEFT JOIN history_ids ON sessions.id = history_ids.session_id
             WHERE (?1 IS NULL OR {HISTORY_DATE} = ?1)
             AND (?2 = '' OR instr(lower(document), lower(?2)) > 0)
             ORDER BY started_at DESC, sessions.rowid DESC LIMIT 100 OFFSET ?3",
        ))?;
        let mut result = statement.query(params![date, query, offset as i64])?;
        let mut rows = Vec::new();
        while let Some(record) = result.next()? {
            let document = record
                .get_ref(0)?
                .as_str()
                .map_err(crate::wire::Error::bad)?;
            let mut row = Row::from(&serde_json::from_str::<Session>(document)?);
            row.history_date = record.get(1)?;
            row.history_id = record.get(2)?;
            rows.push(row);
        }
        Ok(HistoryPage { rows, days })
    }

    pub(crate) fn get_by_history_id(&self, date: &str, id: u64) -> Result<Option<Session>> {
        let document: Option<String> = self
            .db
            .query_row(
                "SELECT document FROM history_ids JOIN sessions ON sessions.id = history_ids.session_id
                 WHERE day = ? AND number = ?",
                params![date, id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(document.as_deref().map(serde_json::from_str).transpose()?)
    }

    pub(crate) fn prepare_history_delete(
        &self,
        target: HistoryDeletion,
    ) -> Result<HistoryDeletePlan> {
        let (field, parameters) = match &target {
            HistoryDeletion::Selected(ids) => ("sessions.id", ids),
            HistoryDeletion::Dates(dates) => (HISTORY_DATE, dates),
        };
        let mut statement = self.db.prepare(&format!(
            "SELECT sessions.id, archived FROM sessions
             LEFT JOIN history_ids ON sessions.id = history_ids.session_id
             WHERE {field} IN (SELECT value FROM json_each(?1))"
        ))?;
        let mut rows = statement.query([serde_json::to_string(parameters)?])?;
        let mut ids = Vec::new();
        let mut active = 0;
        while let Some(row) = rows.next()? {
            ids.push(row.get(0)?);
            active += usize::from(!row.get::<_, bool>(1)?);
        }
        if ids.is_empty() {
            return Err(crate::wire::Error::bad("所选范围已没有可删除的历史记录"));
        }
        let dates = match target {
            HistoryDeletion::Dates(dates) => dates,
            HistoryDeletion::Selected(_) => Vec::new(),
        };
        Ok(HistoryDeletePlan { dates, active, ids })
    }

    pub(crate) fn delete_history(&mut self, plan: HistoryDeletePlan) -> Result<usize> {
        let mut deleted = 0;
        for id in plan.ids {
            if self.is_archived(&id)?.is_some() {
                self.delete(&id)?;
                deleted += 1;
            }
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(id: &str, group: &str, started_at: &str) -> Session {
        Session {
            id: id.into(),
            method: "GET".into(),
            url: format!("http://localhost/{id}"),
            capture_group: group.into(),
            started_at: started_at.into(),
            ..Default::default()
        }
    }

    #[test]
    fn history_ids_increment_after_reopen_and_keep_their_date_scope() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = History::open(dir.path(), 200).unwrap();
        let time = "2026-11-25T12:00:00.000Z";
        for id in ["first", "second"] {
            history.insert(&capture(id, "default", time));
        }

        let first_page = history.desktop_history_page(None, 0, "").unwrap();
        let date = &first_page.days[0].date;
        assert_eq!(first_page.rows[0].id, "second");
        assert_eq!(first_page.rows[0].history_date.as_ref(), Some(date));
        assert_eq!(first_page.rows[0].history_id, Some(2));
        assert_eq!(first_page.rows[1].history_id, Some(1));

        let later = capture("third", "default", time);
        history.insert(&later);
        let page = history.desktop_history_page(Some(date), 0, "").unwrap();
        assert_eq!(page.rows[0].history_id, Some(3));
        let next_page = history.desktop_history_page(Some(date), 1, "").unwrap();
        assert_eq!(next_page.rows[0].history_id, Some(2));
        let filtered = history
            .desktop_history_page(Some(date), 0, "/first")
            .unwrap();
        assert_eq!(filtered.rows[0].history_id, Some(1));
        history.delete(&later.id).unwrap();
        drop(history);

        let mut history = History::open(dir.path(), 200).unwrap();
        history
            .import_records(vec![
                (capture("imported", "default", time), None, None),
                (
                    capture("next-day", "default", "2026-11-26T12:00:00.000Z"),
                    None,
                    None,
                ),
            ])
            .unwrap();
        assert_eq!(
            history.get_by_history_id(date, 1).unwrap().unwrap().id,
            "first"
        );
        assert!(history.get_by_history_id(date, 3).unwrap().is_none());
        assert_eq!(
            history.get_by_history_id(date, 4).unwrap().unwrap().id,
            "imported"
        );
        let page = history.desktop_history_page(None, 0, "/next-day").unwrap();
        assert_ne!(page.rows[0].history_date.as_ref(), Some(date));
        assert_eq!(page.rows[0].history_id, Some(1));
    }

    #[test]
    fn desktop_retains_a_full_round_and_deletes_the_confirmed_history_scope() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = History::open(dir.path(), 200).unwrap();
        history.enable_desktop_captures().unwrap();
        let time = "2026-11-25T12:00:00.000Z";
        let oldest = capture("oldest", "default", time);
        history.insert(&oldest);
        for index in 0..205 {
            history.insert(&capture(&format!("request-{index}"), "default", time));
        }
        let mut private = capture("private", "default", time);
        private.incognito = true;
        history.insert(&private);
        let other = capture("other", "other-tab", time);
        history.insert(&other);
        let mut completed = oldest.clone();
        completed.status = 200;
        completed.response_body_complete = Some(true);
        fs::write(history.body_path(&oldest.id, "response"), b"body").unwrap();
        history.save(&completed);
        let initial = history
            .take_capture_batch("default", true)
            .unwrap()
            .unwrap();
        assert_eq!(initial.rows.len(), 207);
        assert_eq!(initial.rows["oldest"].status, 200);
        assert!(initial.rows["private"].incognito);
        let selected = history
            .prepare_history_delete(HistoryDeletion::Selected(vec![
                oldest.id.clone(),
                private.id.clone(),
            ]))
            .unwrap();
        assert_eq!((selected.count(), selected.active), (1, 1));
        assert_eq!(history.archive_captures("default", None).unwrap(), (206, 1));
        assert!(
            history
                .take_capture_batch("default", false)
                .unwrap()
                .unwrap()
                .rows
                .is_empty()
        );
        assert!(history.get(&private.id).unwrap().is_none());
        assert_eq!(history.body(&oldest.id, "response").unwrap().0, b"body");
        history.save(&completed);
        assert!(
            history
                .take_capture_batch("default", false)
                .unwrap()
                .is_none()
        );
        drop(history);

        let mut reopened = History::open(dir.path(), 200).unwrap();
        reopened.enable_desktop_captures().unwrap();
        assert!(
            reopened
                .take_capture_batch("default", true)
                .unwrap()
                .unwrap()
                .rows
                .is_empty()
        );
        assert_eq!(
            reopened
                .take_capture_batch("other-tab", true)
                .unwrap()
                .unwrap()
                .rows
                .len(),
            1
        );
        let days = reopened.desktop_history_page(None, 0, "").unwrap().days;
        assert_eq!(days.len(), 1);
        let day = &days[0];
        assert_eq!(day.total, 207);
        let filtered = reopened
            .desktop_history_page(Some(&day.date), 0, "/oldest")
            .unwrap();
        assert_eq!(filtered.rows.len(), 1);
        let first_plan = reopened
            .prepare_history_delete(HistoryDeletion::Selected(vec![other.id.clone()]))
            .unwrap();
        assert_eq!((first_plan.count(), first_plan.active), (1, 1));
        assert_eq!(reopened.delete_history(first_plan).unwrap(), 1);
        assert!(reopened.get(&other.id).unwrap().is_none());
        let additional_day = capture("second-day", "other-tab", "2026-11-26T12:00:00.000Z");
        reopened.insert(&additional_day);
        let days = reopened.desktop_history_page(None, 0, "").unwrap().days;
        assert_eq!(days.len(), 2);
        let plan = reopened
            .prepare_history_delete(HistoryDeletion::Dates(
                days.into_iter().map(|day| day.date).collect(),
            ))
            .unwrap();
        assert_eq!((plan.count(), plan.active), (207, 1));
        let late = capture("after-confirmation-opened", "default", time);
        reopened.insert(&late);
        assert_eq!(reopened.delete_history(plan).unwrap(), 207);
        assert!(!reopened.body_path(&oldest.id, "response").exists());
        assert!(reopened.get(&additional_day.id).unwrap().is_none());
        assert!(reopened.get(&late.id).unwrap().is_some());
    }

    #[test]
    fn previous_workspace_cutoffs_and_removed_rows_migrate() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = History::open(dir.path(), 200).unwrap();
        let old = "2026-11-24T10:00:00.000Z";
        let current = "2026-11-25T10:00:00.000Z";
        for capture in [
            capture("cleared", "default", old),
            capture("removed", "default", current),
            capture("active", "default", current),
            capture("closed-tab", "closed", current),
        ] {
            history.insert(&capture);
        }
        fs::write(
            dir.path().join("desktop-captures.json"),
            format!(r#"{{"default":{{"cleared_through":"{old}","hidden":["removed"]}}}}"#),
        )
        .unwrap();
        history.enable_desktop_captures().unwrap();
        assert_eq!(
            history
                .take_capture_batch("default", true)
                .unwrap()
                .unwrap()
                .rows
                .into_keys()
                .collect::<Vec<_>>(),
            ["active"]
        );
        for id in ["cleared", "removed", "closed-tab"] {
            assert_eq!(history.is_archived(id).unwrap(), Some(true));
        }
    }
}
