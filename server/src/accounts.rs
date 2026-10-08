//! Accounts: usernames, passwords, sessions and invite codes. New accounts need an invite code; after that, people
//! sign in with their username and password on any device.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use rand::{Rng, RngCore};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::auth;

/// Usernames: 3 to 20 letters, digits or underscores. Unique ignoring case.
pub fn valid_username(name: &str) -> bool {
    (3..=20).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

pub fn valid_password(pw: &str) -> bool {
    (8..=128).contains(&pw.chars().count())
}

fn hash_password(pw: &str) -> String {
    let mut salt = [0u8; 16];
    rand::rng().fill_bytes(&mut salt);
    let salt = SaltString::encode_b64(&salt).expect("16 bytes is a valid salt");
    Argon2::default().hash_password(pw.as_bytes(), &salt).expect("argon2 accepts any password").to_string()
}

fn verify_password(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
}

/// Password hashing is slow on purpose (so stolen hashes are slow to crack); keep it off the request threads.
pub async fn hash(pw: String) -> String {
    tokio::task::spawn_blocking(move || hash_password(&pw)).await.expect("hashing doesn't panic")
}

pub async fn verify(pw: String, hash: String) -> bool {
    tokio::task::spawn_blocking(move || verify_password(&pw, &hash)).await.expect("verifying doesn't panic")
}

fn random_from(alphabet: &[u8], n: usize) -> String {
    let mut rng = rand::rng();
    (0..n).map(|_| alphabet[rng.random_range(0..alphabet.len())] as char).collect()
}

/// Groups of 4 with dashes, for reading out or typing.
fn grouped(s: &str) -> String {
    s.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join("-")
}

/// A new invite code: 12 characters with no look-alikes (no I, L, O, 0 or 1), about 59 bits. Stored without dashes.
pub fn new_invite_code() -> String {
    random_from(b"ABCDEFGHJKMNPQRSTUVWXYZ23456789", 12)
}

/// How an invite code is shown: ABCD-EFGH-JKMN.
pub fn show_code(code: &str) -> String {
    grouped(code)
}

/// What people type: any case, with or without dashes or spaces.
pub fn normalize_code(input: &str) -> String {
    input.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).collect()
}

/// A generated password for the admin account and for password resets: 16 lower-case characters, about 79 bits.
pub fn new_password() -> String {
    grouped(&random_from(b"abcdefghjkmnpqrstuvwxyz23456789", 16))
}

/// Makes an account with its starting packs. The username must be valid; a taken one fails the insert.
pub async fn create(
    c: &mut PgConnection,
    username: &str,
    password_hash: &str,
    invite: Option<&str>,
    admin: bool,
    start_pack: &str,
    start_packs: i32,
) -> sqlx::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "insert into users (id, username, password_hash, invite_code, is_admin, next_claim_at)
         values ($1, $2, $3, $4, $5, now())",
    )
    .bind(id)
    .bind(username)
    .bind(password_hash)
    .bind(invite)
    .bind(admin)
    .execute(&mut *c)
    .await?;
    sqlx::query("insert into user_packs (user_id, pack_id, sealed) values ($1, $2, $3)")
        .bind(id)
        .bind(start_pack)
        .bind(start_packs)
        .execute(&mut *c)
        .await?;
    Ok(id)
}

/// Signs the account in on one more device and returns the cookie token.
pub async fn new_session(c: &mut PgConnection, user: Uuid) -> sqlx::Result<String> {
    let token = auth::new_token();
    sqlx::query("insert into sessions (token_hash, user_id) values ($1, $2)")
        .bind(auth::hash(&token))
        .bind(user)
        .execute(c)
        .await?;
    Ok(token)
}

pub fn is_unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error().is_some_and(|d| d.is_unique_violation())
}

/// Counts failed sign-ins and invite codes, per username and per visitor, so passwords and codes can't be guessed by
/// brute force. Kept in memory: a restart clears it, which is fine for slowing guessing down.
#[derive(Default)]
pub struct Limiter {
    fails: Mutex<HashMap<String, (u32, Instant)>>,
}

const WINDOW: Duration = Duration::from_secs(15 * 60);

impl Limiter {
    /// True once `key` has failed `max` times within the last 15 minutes.
    pub fn blocked(&self, key: &str, max: u32) -> bool {
        let fails = self.fails.lock().unwrap();
        fails.get(key).is_some_and(|(n, since)| *n >= max && since.elapsed() < WINDOW)
    }

    pub fn fail(&self, key: &str) {
        let mut fails = self.fails.lock().unwrap();
        if fails.len() > 10_000 {
            fails.retain(|_, (_, since)| since.elapsed() < WINDOW);
        }
        let e = fails.entry(key.to_string()).or_insert((0, Instant::now()));
        if e.1.elapsed() >= WINDOW {
            *e = (0, Instant::now());
        }
        e.0 += 1;
    }

    pub fn clear(&self, key: &str) {
        self.fails.lock().unwrap().remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_hash_and_verify() {
        let h = hash_password("correct horse");
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("correct horse", &h));
        assert!(!verify_password("wrong horse", &h));
        assert!(!verify_password("correct horse", "not a hash"));
    }

    #[test]
    fn codes_are_easy_to_type() {
        let c = new_invite_code();
        assert_eq!(c.len(), 12);
        assert!(!c.contains(['I', 'L', 'O', '0', '1']));
        assert_eq!(normalize_code(&show_code(&c).to_lowercase()), c);
        assert_eq!(normalize_code(" abcd efgh-jkmn "), "ABCDEFGHJKMN");
        assert_eq!(new_password().len(), 19);
    }

    #[test]
    fn usernames() {
        assert!(valid_username("cassie_7028"));
        for bad in ["ab", "a".repeat(21).as_str(), "has space", "dash-name", "émile"] {
            assert!(!valid_username(bad), "{bad}");
        }
    }

    #[test]
    fn limiter_blocks_after_max() {
        let l = Limiter::default();
        for _ in 0..3 {
            assert!(!l.blocked("k", 3));
            l.fail("k");
        }
        assert!(l.blocked("k", 3));
        assert!(!l.blocked("other", 3));
        l.clear("k");
        assert!(!l.blocked("k", 3));
    }
}
