//! 最小的 Anki `.apkg` 写出器：SQLite `collection.anki2` + 不压缩的 ZIP 容器。
//!
//! 只用现有依赖，不引 zip/sha1 库：ZIP 条目用 stored（method 0），Anki 照常导入；
//! 校验字段 `csum` 需要 SHA-1，这里按 FIPS 180-1 实现一份，配已知向量测试。
//! 生成的是 Anki 2.1 仍能读的 schema ver 11，导入后会由 Anki 自行升级。

use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection};
use serde_json::json;
use std::io::Write;
use std::path::Path;

pub struct Note {
    pub front: String,
    pub back: String,
    pub tags: Vec<String>,
}

/// 写入 `.apkg`，返回卡片数。先写同目录临时文件，成功后 rename。
pub fn write_apkg(out: &Path, deck: &str, notes: &[Note]) -> Result<usize> {
    ensure!(!deck.trim().is_empty(), "牌组名不能为空");
    ensure!(deck.chars().count() <= 100, "牌组名最多 100 字符");
    ensure!(!deck.chars().any(char::is_control), "牌组名不能含控制字符");
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("创建输出目录: {}", parent.display()))?;

    let collection = collection_bytes(deck, notes)?;
    let zip = zip_stored(&[("collection.anki2", collection), ("media", b"{}".to_vec())]);
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(&zip)?;
    tmp.as_file().sync_all()?;
    tmp.persist(out).map_err(|e| e.error)?;
    Ok(notes.len())
}

fn collection_bytes(deck: &str, notes: &[Note]) -> Result<Vec<u8>> {
    let file = tempfile::Builder::new().suffix(".anki2").tempfile()?;
    let conn = Connection::open(file.path())?;
    conn.execute_batch(
        "CREATE TABLE col (
             id integer primary key, crt integer not null, mod integer not null,
             scm integer not null, ver integer not null, dty integer not null,
             usn integer not null, ls integer not null, conf text not null,
             models text not null, decks text not null, dconf text not null,
             tags text not null);
         CREATE TABLE notes (
             id integer primary key, guid text not null, mid integer not null,
             mod integer not null, usn integer not null, tags text not null,
             flds text not null, sfld integer not null, csum integer not null,
             flags integer not null, data text not null);
         CREATE TABLE cards (
             id integer primary key, nid integer not null, did integer not null,
             ord integer not null, mod integer not null, usn integer not null,
             type integer not null, queue integer not null, due integer not null,
             ivl integer not null, factor integer not null, reps integer not null,
             lapses integer not null, left integer not null, odue integer not null,
             odid integer not null, flags integer not null, data text not null);
         CREATE TABLE revlog (
             id integer primary key, cid integer not null, usn integer not null,
             ease integer not null, ivl integer not null, lastIvl integer not null,
             factor integer not null, time integer not null, type integer not null);
         CREATE TABLE graves (usn integer not null, oid integer not null, type integer not null);
         CREATE INDEX ix_notes_usn ON notes (usn);
         CREATE INDEX ix_cards_usn ON cards (usn);
         CREATE INDEX ix_revlog_usn ON revlog (usn);
         CREATE INDEX ix_cards_nid ON cards (nid);
         CREATE INDEX ix_cards_sched ON cards (did, queue, due);
         CREATE INDEX ix_revlog_cid ON revlog (cid);
         CREATE INDEX ix_notes_csum ON notes (csum);",
    )?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let now_ms = now * 1000;
    let models = json!({
        "1": {
            "id": 1, "name": "Basic (zedict)", "type": 0, "mod": 0, "usn": 0,
            "sortf": 0, "did": 1,
            "tmpls": [{
                "name": "Card 1", "ord": 0,
                "qfmt": "{{Front}}",
                "afmt": "{{FrontSide}}<hr id=answer>{{Back}}",
                "bqfmt": "", "bafmt": "", "did": null, "bfont": "", "bsize": 0
            }],
            "flds": [
                {"name": "Front", "ord": 0, "sticky": false, "rtl": false,
                 "font": "Arial", "size": 20, "media": []},
                {"name": "Back", "ord": 1, "sticky": false, "rtl": false,
                 "font": "Arial", "size": 20, "media": []}
            ],
            "css": ".card { font-family: arial; font-size: 20px; text-align: center; \
                    color: black; background-color: white; }",
            "latexPre": "", "latexPost": "", "req": [[0, "any", [0]]]
        }
    })
    .to_string();
    let decks = json!({
        "1": {
            "id": 1, "name": deck, "mod": 0, "usn": 0, "desc": "", "dyn": 0,
            "collapsed": false, "conf": 1,
            "newToday": [0, 0], "revToday": [0, 0], "lrnToday": [0, 0],
            "timeToday": [0, 0], "extendNew": 10, "extendRev": 50,
            "browserCollapsed": false
        }
    })
    .to_string();
    let dconf = json!({
        "1": {
            "id": 1, "name": "Default", "mod": 0, "usn": 0, "maxTaken": 60,
            "autoplay": true, "timer": 0, "replayq": true, "dyn": false,
            "new": {"delays": [1, 10], "ints": [1, 4, 7], "initialFactor": 2500,
                    "order": 1, "perDay": 20, "bury": false, "separate": true},
            "rev": {"perDay": 200, "ease4": 1.3, "fuzz": 0.05, "minSpace": 1,
                    "ivlFct": 1, "maxIvl": 36500, "bury": false, "hardFactor": 1.2},
            "lapse": {"delays": [10], "mult": 0, "minInt": 1, "leechFails": 8,
                      "leechAction": 1, "leech": 0}
        }
    })
    .to_string();
    let conf = json!({
        "nextPos": notes.len() + 1, "estTimes": true, "activeDecks": [1],
        "curDeck": 1, "newSpread": 0, "dueCounts": true, "curModel": 1,
        "collapseTime": 1200, "timeLim": 0
    })
    .to_string();
    conn.execute(
        "INSERT INTO col VALUES (1, ?1, ?2, ?2, 11, 0, 0, 0, ?3, ?4, ?5, ?6, '{}')",
        params![now, now_ms, conf, models, decks, dconf],
    )?;

    let tx = conn.unchecked_transaction()?;
    {
        let mut note_stmt =
            tx.prepare("INSERT INTO notes VALUES (?1, ?2, 1, ?3, 0, ?4, ?5, ?6, ?7, 0, '')")?;
        let mut card_stmt = tx.prepare(
            "INSERT INTO cards VALUES (?1, ?1, 1, 0, ?2, 0, 0, 0, ?3, 0, 2500, 0, 0, 0, 0, 0, 0, '')",
        )?;
        for (index, note) in notes.iter().enumerate() {
            let id = index as i64 + 1;
            let front = html_field(&note.front);
            let back = html_field(&note.back);
            let tags = if note.tags.is_empty() {
                " zedict ".to_string()
            } else {
                format!(" {} ", note.tags.join(" "))
            };
            let guid = crate::data::ingest::hash_bytes(
                format!("zedict\x1f{id}\x1f{}", note.front).as_bytes(),
            );
            let guid = guid.chars().take(16).collect::<String>();
            let csum = i64::from(field_checksum(&note.front));
            note_stmt.execute(params![
                id,
                guid,
                now,
                tags,
                format!("{front}\x1f{back}"),
                note.front.as_str(),
                csum
            ])?;
            card_stmt.execute(params![id, now, id])?;
        }
    }
    tx.commit()?;
    drop(conn);
    std::fs::read(file.path()).context("读取临时 Anki 库")
}

fn html_field(text: &str) -> String {
    html_escape::encode_text(text).replace('\n', "<br>")
}

/// Anki 的字段校验和：SHA-1(字段文本) 的前 8 个十六进制位当作整数。
fn field_checksum(text: &str) -> u32 {
    u32::from_str_radix(&sha1_hex(text.as_bytes())[..8], 16).unwrap_or(0)
}

fn sha1_hex(data: &[u8]) -> String {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (i, slot) in w.iter_mut().take(16).enumerate() {
            *slot = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// 只用 stored 条目的 ZIP 容器；文件时间固定为 1980-01-01，输出可复现。
fn zip_stored(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let offset = body.len() as u32;
        let crc = crc32(data);
        let size = data.len() as u32;
        body.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        body.extend_from_slice(&20u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0x21u16.to_le_bytes());
        body.extend_from_slice(&crc.to_le_bytes());
        body.extend_from_slice(&size.to_le_bytes());
        body.extend_from_slice(&size.to_le_bytes());
        body.extend_from_slice(&(name.len() as u16).to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(name.as_bytes());
        body.extend_from_slice(data);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0x21u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset = body.len() as u32;
    let central_size = central.len() as u32;
    body.extend_from_slice(&central);
    body.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    body.extend_from_slice(&0u16.to_le_bytes());
    body.extend_from_slice(&0u16.to_le_bytes());
    body.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    body.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    body.extend_from_slice(&central_size.to_le_bytes());
    body.extend_from_slice(&central_offset.to_le_bytes());
    body.extend_from_slice(&0u16.to_le_bytes());
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_matches_the_standard_vectors() {
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha1_hex(b"The quick brown fox jumps over the lazy dog"),
            "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
        );
    }

    #[test]
    fn crc32_matches_the_standard_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn zip_entries_are_stored_and_extractable() {
        let bytes = zip_stored(&[("a.txt", b"hello".to_vec()), ("b.txt", vec![1, 2, 3])]);
        assert_eq!(&bytes[..4], &0x0403_4b50u32.to_le_bytes());
        assert_eq!(extract(&bytes, "a.txt").unwrap(), b"hello");
        assert_eq!(extract(&bytes, "b.txt").unwrap(), [1, 2, 3]);
        assert!(extract(&bytes, "missing").is_none());
        assert_eq!(
            &bytes[bytes.len() - 22..bytes.len() - 18],
            &0x0605_4b50u32.to_le_bytes()
        );
    }

    #[test]
    fn package_contains_a_readable_collection() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("deck.apkg");
        let notes = vec![
            Note {
                front: "apple".into(),
                back: "n. 苹果".into(),
                tags: vec!["zedict".into(), "pos-n".into()],
            },
            Note {
                front: "a & b".into(),
                back: "line1\nline2".into(),
                tags: vec![],
            },
        ];
        assert_eq!(write_apkg(&out, "我的词书", &notes).unwrap(), 2);
        let bytes = std::fs::read(&out).unwrap();
        let collection = extract(&bytes, "collection.anki2").unwrap();
        assert_eq!(extract(&bytes, "media").unwrap(), b"{}");

        let db = dir.path().join("check.sqlite");
        std::fs::write(&db, collection).unwrap();
        let conn = Connection::open(&db).unwrap();
        let (ver, cards): (i64, i64) = conn
            .query_row(
                "SELECT (SELECT ver FROM col), (SELECT count(*) FROM cards)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(ver, 11);
        assert_eq!(cards, 2);
        let (flds, sfld): (String, String) = conn
            .query_row("SELECT flds, sfld FROM notes WHERE id=1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(sfld, "apple");
        assert!(flds.starts_with("apple\x1f"), "{flds}");
        assert!(flds.contains("n. 苹果"));
        let escaped: String = conn
            .query_row("SELECT flds FROM notes WHERE id=2", [], |r| r.get(0))
            .unwrap();
        assert!(escaped.starts_with("a &amp; b\x1f"), "{escaped}");
        assert!(escaped.contains("line1<br>line2"), "{escaped}");
        let guid: String = conn
            .query_row("SELECT guid FROM notes WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(guid.len(), 16);
        assert!(guid.chars().all(|c| c.is_ascii_hexdigit()));
    }

    fn extract(bytes: &[u8], want: &str) -> Option<Vec<u8>> {
        let mut pos = 0usize;
        while bytes.get(pos..pos + 4)? == 0x0403_4b50u32.to_le_bytes() {
            let name_len =
                u16::from_le_bytes(bytes.get(pos + 26..pos + 28)?.try_into().ok()?) as usize;
            let extra_len =
                u16::from_le_bytes(bytes.get(pos + 28..pos + 30)?.try_into().ok()?) as usize;
            let size = u32::from_le_bytes(bytes.get(pos + 18..pos + 22)?.try_into().ok()?) as usize;
            let name = std::str::from_utf8(bytes.get(pos + 30..pos + 30 + name_len)?).ok()?;
            let start = pos + 30 + name_len + extra_len;
            let data = bytes.get(start..start + size)?;
            if name == want {
                return Some(data.to_vec());
            }
            pos = start + size;
        }
        None
    }
}
