use rusqlite::Connection;

use serde::{Deserialize, Serialize};
use serde_rusqlite::from_row;
use skillratings::{
    Outcomes,
    glicko2::{Glicko2Config, Glicko2Rating, glicko2_rating_period},
};
#[derive(Deserialize, Serialize)]
struct RatingRow {
    solved: bool,
    username: String,
    rating: f64,
}

pub fn rating_for_puzzles(db_conn: &Connection, puzzle_id: i64) -> anyhow::Result<Glicko2Rating> {
    let mut stmt = db_conn.prepare("WITH ranked_attempts AS (
    SELECT *,
           ROW_NUMBER() OVER (
               PARTITION BY username, puzzle_id
               ORDER BY timestamp_seconds ASC
           ) AS rn
    FROM puzzle_attempts
        )
    SELECT ranked_attempts.solved, users.username, users.rating
    FROM ranked_attempts JOIN users ON ranked_attempts.username = users.username
    WHERE puzzle_id = ?1 AND rn = 1 AND ranked_attempts.username != 'Morten' AND ranked_attempts.username != 'Mort2'
")?;
    let ratings: Vec<RatingRow> = stmt
        .query_and_then([puzzle_id], from_row::<RatingRow>)?
        .collect::<Result<Vec<_>, _>>()?;

    let puzzle_default_rating = default_puzzle_rating(db_conn, puzzle_id)?;

    let puzzle_player = Glicko2Rating {
        rating: puzzle_default_rating as f64,
        ..Default::default()
    };

    let results = ratings
        .into_iter()
        .map(|r| {
            let player_rating = Glicko2Rating {
                rating: r.rating,
                ..Default::default()
            };
            if r.solved {
                (player_rating, Outcomes::LOSS)
            } else {
                (player_rating, Outcomes::WIN)
            }
        })
        .collect::<Vec<_>>();

    let new_player = glicko2_rating_period(&puzzle_player, &results, &Glicko2Config::new());

    Ok(new_player)
}

pub fn default_puzzle_rating(db_conn: &Connection, puzzle_id: i64) -> anyhow::Result<f64> {
    let solution = db_conn
        .query_row(
            "SELECT solution FROM puzzles WHERE id = ?1",
            [puzzle_id],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_default();

    let puzzle_default_rating = 1250 + (solution.split_whitespace().count() / 2) * 350;

    Ok(puzzle_default_rating as f64)
}
