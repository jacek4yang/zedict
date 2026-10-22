//! 本地学习闭环：生词本、复习计划与统计，独立于词典库。
//!
//! 参照欧陆 `study.db` 的形状（词条、熟练度、复习记录），但只实现离线必需的三张表。
//! 路径默认是用户数据目录下的 `study.sqlite`，可用 `ZEDICT_STUDY_DB` 重定向；
//! 词典库永远只读，学习状态不写回词库。

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub fn default_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("ZEDICT_STUDY_DB") {
        return Ok(PathBuf::from(path));
    }
    Ok(crate::app::config::dirs()?.data_dir().join("study.sqlite"))
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Grade {
    Again,
    Hard,
    Good,
    Easy,
}

impl Grade {
    pub fn label(self) -> &'static str {
        match self {
            Self::Again => "again",
            Self::Hard => "hard",
            Self::Good => "good",
            Self::Easy => "easy",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StudyWord {
    pub key: String,
    pub headword: String,
    pub added_at: i64,
    pub due_at: i64,
    pub interval_days: f64,
    pub ease: f64,
    pub reps: i64,
    pub lapses: i64,
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub total: i64,
    pub new: i64,
    pub learning: i64,
    pub review: i64,
    pub due: i64,
    pub reviews: i64,
    pub last_review_at: Option<i64>,
}

pub struct StudyStore {
    conn: Connection,
}

impl StudyStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建学习数据目录: {}", parent.display()))?;
        }
        let conn =
            Connection::open(path).with_context(|| format!("打开学习数据: {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS study_word(
                 key TEXT PRIMARY KEY,
                 headword TEXT NOT NULL,
                 added_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL,
                 due_at INTEGER NOT NULL,
                 interval_days REAL NOT NULL,
                 ease REAL NOT NULL,
                 reps INTEGER NOT NULL,
                 lapses INTEGER NOT NULL,
                 state TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS study_word_due ON study_word(due_at, key);
             CREATE TABLE IF NOT EXISTS review_log(
                 key TEXT NOT NULL,
                 at INTEGER NOT NULL,
                 grade TEXT NOT NULL,
                 interval_days REAL NOT NULL
             );
             CREATE INDEX IF NOT EXISTS review_log_at ON review_log(at);",
        )?;
        Ok(Self { conn })
    }

    /// 返回是否新加入；已在生词本里时保持原进度不动。
    pub fn add(&self, key: &str, headword: &str, at: i64) -> Result<bool> {
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO study_word
             (key, headword, added_at, updated_at, due_at, interval_days, ease, reps, lapses, state)
             VALUES (?1, ?2, ?3, ?3, ?3, 0, 2.5, 0, 0, 'new')",
            params![key, headword, at],
        )?;
        Ok(inserted > 0)
    }

    pub fn remove(&self, key: &str) -> Result<bool> {
        let removed = self
            .conn
            .execute("DELETE FROM study_word WHERE key=?1", [key])?;
        Ok(removed > 0)
    }

    pub fn get(&self, key: &str) -> Result<Option<StudyWord>> {
        Ok(self
            .conn
            .query_row(
                "SELECT key, headword, added_at, due_at, interval_days, ease, reps, lapses, state
                 FROM study_word WHERE key=?1",
                [key],
                row_word,
            )
            .optional()?)
    }

    pub fn list(&self, due_only: bool, limit: usize, at: i64) -> Result<Vec<StudyWord>> {
        let mut stmt = self.conn.prepare(
            "SELECT key, headword, added_at, due_at, interval_days, ease, reps, lapses, state
             FROM study_word
             WHERE (?1=0 OR due_at<=?2)
             ORDER BY due_at, key LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![due_only as i64, at, limit as i64], row_word)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn all(&self) -> Result<Vec<StudyWord>> {
        let mut stmt = self.conn.prepare(
            "SELECT key, headword, added_at, due_at, interval_days, ease, reps, lapses, state
             FROM study_word ORDER BY added_at, key",
        )?;
        let rows = stmt.query_map([], row_word)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn review(&self, key: &str, grade: Grade, at: i64) -> Result<StudyWord> {
        let mut word = self
            .get(key)?
            .with_context(|| format!("生词本里没有 {key}；先 zedict study add <词>"))?;
        schedule(&mut word, grade, at);
        self.conn.execute(
            "UPDATE study_word SET updated_at=?2, due_at=?3, interval_days=?4, ease=?5,
             reps=?6, lapses=?7, state=?8 WHERE key=?1",
            params![
                word.key,
                at,
                word.due_at,
                word.interval_days,
                word.ease,
                word.reps,
                word.lapses,
                word.state
            ],
        )?;
        self.conn.execute(
            "INSERT INTO review_log(key, at, grade, interval_days) VALUES (?1,?2,?3,?4)",
            params![word.key, at, grade_label(grade), word.interval_days],
        )?;
        Ok(word)
    }

    pub fn stats(&self, at: i64) -> Result<Stats> {
        let count = |sql: &str| -> Result<i64> { Ok(self.conn.query_row(sql, [], |r| r.get(0))?) };
        Ok(Stats {
            total: count("SELECT count(*) FROM study_word")?,
            new: count("SELECT count(*) FROM study_word WHERE state='new'")?,
            learning: count("SELECT count(*) FROM study_word WHERE state='learning'")?,
            review: count("SELECT count(*) FROM study_word WHERE state='review'")?,
            due: self.conn.query_row(
                "SELECT count(*) FROM study_word WHERE due_at<=?1",
                [at],
                |row| row.get(0),
            )?,
            reviews: count("SELECT count(*) FROM review_log")?,
            last_review_at: self
                .conn
                .query_row("SELECT max(at) FROM review_log", [], |r| r.get(0))
                .optional()?
                .flatten(),
        })
    }
}

fn row_word(row: &rusqlite::Row<'_>) -> rusqlite::Result<StudyWord> {
    Ok(StudyWord {
        key: row.get(0)?,
        headword: row.get(1)?,
        added_at: row.get(2)?,
        due_at: row.get(3)?,
        interval_days: row.get(4)?,
        ease: row.get(5)?,
        reps: row.get(6)?,
        lapses: row.get(7)?,
        state: row.get(8)?,
    })
}

fn grade_label(grade: Grade) -> &'static str {
    grade.label()
}

/// 简化版 SM-2：again 退回 10 分钟后重来；hard/good/easy 按 ease 放大间隔。
/// 不追求与欧陆评分逐位一致，只保证同样输入得到同样计划，且时间只进不退。
pub fn schedule(word: &mut StudyWord, grade: Grade, at: i64) {
    const DAY: f64 = 86_400.0;
    match grade {
        Grade::Again => {
            word.lapses += 1;
            word.ease = (word.ease - 0.20).max(1.3);
            word.interval_days = 0.0;
            word.due_at = at + 600;
            word.state = "learning".into();
        }
        Grade::Hard => {
            word.ease = (word.ease - 0.15).max(1.3);
            word.interval_days = if word.interval_days < 1.0 {
                1.0
            } else {
                word.interval_days * 1.2
            };
            word.due_at = at + (word.interval_days * DAY).round() as i64;
            word.state = "review".into();
        }
        Grade::Good => {
            word.interval_days = if word.interval_days < 1.0 {
                1.0
            } else {
                word.interval_days * word.ease
            };
            word.due_at = at + (word.interval_days * DAY).round() as i64;
            word.state = "review".into();
        }
        Grade::Easy => {
            word.ease = (word.ease + 0.15).min(3.5);
            word.interval_days = if word.interval_days < 1.0 {
                4.0
            } else {
                word.interval_days * word.ease * 1.3
            };
            word.due_at = at + (word.interval_days * DAY).round() as i64;
            word.state = "review".into();
        }
    }
    word.interval_days = (word.interval_days * 100.0).round() / 100.0;
    word.reps += 1;
}

/// 人类可读的下次复习时间，不引日期库：按与“现在”的距离显示。
pub fn due_label(due_at: i64, now: i64) -> String {
    let diff = due_at - now;
    if diff <= 0 {
        "已到期".into()
    } else if diff < 3600 {
        format!("{} 分钟后", (diff + 59) / 60)
    } else if diff < 86_400 {
        format!("{} 小时后", (diff + 3599) / 3600)
    } else {
        format!("{} 天后", (diff + 86_399) / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word() -> StudyWord {
        StudyWord {
            key: "apple".into(),
            headword: "apple".into(),
            added_at: 0,
            due_at: 0,
            interval_days: 0.0,
            ease: 2.5,
            reps: 0,
            lapses: 0,
            state: "new".into(),
        }
    }

    #[test]
    fn good_grows_the_interval_and_again_lapses_it() {
        let mut w = word();
        schedule(&mut w, Grade::Good, 1_000);
        assert_eq!(w.interval_days, 1.0);
        assert_eq!(w.due_at, 1_000 + 86_400);
        assert_eq!(w.state, "review");
        schedule(&mut w, Grade::Good, 2_000);
        assert_eq!(w.interval_days, 2.5);
        schedule(&mut w, Grade::Again, 3_000);
        assert_eq!(w.interval_days, 0.0);
        assert_eq!(w.due_at, 3_600);
        assert_eq!(w.lapses, 1);
        assert_eq!(w.state, "learning");
        assert!((w.ease - 2.3).abs() < 1e-9, "ease={}", w.ease);
    }

    #[test]
    fn easy_starts_at_four_days_and_ease_stays_bounded() {
        let mut w = word();
        schedule(&mut w, Grade::Easy, 0);
        assert_eq!(w.interval_days, 4.0);
        for _ in 0..20 {
            schedule(&mut w, Grade::Easy, 0);
        }
        assert_eq!(w.ease, 3.5);
        assert!(w.reps == 21);
    }

    #[test]
    fn due_labels_stay_short() {
        assert_eq!(due_label(100, 100), "已到期");
        assert_eq!(due_label(161, 100), "2 分钟后");
        assert_eq!(due_label(3_700, 100), "1 小时后");
        assert_eq!(due_label(100 + 86_400 * 2 - 1, 100), "2 天后");
    }

    #[test]
    fn add_is_idempotent_and_review_requires_membership() {
        let dir = tempfile::tempdir().unwrap();
        let store = StudyStore::open(&dir.path().join("study.sqlite")).unwrap();
        assert!(store.add("apple", "apple", 10).unwrap());
        assert!(!store.add("apple", "apple", 20).unwrap());
        let saved = store.get("apple").unwrap().unwrap();
        assert_eq!(saved.added_at, 10, "重复 add 不改首次加入时间");
        assert!(store.review("missing", Grade::Good, 30).is_err());
        let reviewed = store.review("apple", Grade::Good, 30).unwrap();
        assert_eq!(reviewed.reps, 1);
        assert_eq!(store.stats(30).unwrap().due, 0, "刚复习完的词当天不再到期");
        assert_eq!(store.stats(30 + 86_400).unwrap().due, 1);
        assert_eq!(store.stats(30).unwrap().reviews, 1);
    }
}
