//! One-off local migration: rebuild gloss_en from archived source records.
//! Not part of the shipped CLI; safe to delete after use.

use anyhow::{Context, Result};
use flate2::read::ZlibDecoder;
use rusqlite::{params, Connection};
use std::io::Read;

fn main() -> Result<()> {
    let db = std::env::args()
        .nth(1)
        .context("usage: cargo run --example migrate_gloss -- <db.sqlite>")?;
    let mut conn = Connection::open(&db)?;
    let mut last = 0i64;
    let mut checked = 0usize;
    let mut changed = 0usize;

    loop {
        let batch: Vec<(i64, Vec<u8>)> = {
            let mut stmt =
                conn.prepare("SELECT id, card FROM entries WHERE id > ?1 ORDER BY id LIMIT 2000")?;
            let rows = stmt
                .query_map([last], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        if batch.is_empty() {
            break;
        }
        let tx = conn.transaction()?;
        for (id, blob) in batch {
            last = id;
            checked += 1;
            let mut card = zedict::data::compile::decompress_card(&blob)?;
            let Some(gloss) = card.gloss_en.as_deref() else {
                continue;
            };
            if gloss.contains('\n') {
                continue;
            }
            let raw: Vec<u8> = tx.query_row(
                "SELECT raw FROM source_records WHERE line=?1",
                [id],
                |row| row.get(0),
            )?;
            let mut decoded = Vec::new();
            ZlibDecoder::new(raw.as_slice()).read_to_end(&mut decoded)?;
            let value: serde_json::Value = serde_json::from_slice(&decoded)?;
            let Some(html) = value.get("english").and_then(|v| v.as_str()) else {
                continue;
            };
            let parsed = zedict::data::compile::parse_english_gloss(html);
            if parsed == gloss {
                continue;
            }
            card.gloss_en = Some(parsed);
            let packed = zedict::data::compile::pack_compressed(&serde_json::to_vec(&card)?)?;
            tx.execute(
                "UPDATE entries SET card=?1 WHERE id=?2",
                params![packed, id],
            )?;
            changed += 1;
        }
        tx.commit()?;
        eprintln!("checked={checked} changed={changed} last={last}");
    }
    println!("checked={checked} changed={changed}");
    Ok(())
}
