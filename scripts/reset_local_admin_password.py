#!/usr/bin/env python3
"""One-shot local admin password reset for Buz example DB (WAL sqlite).

Reads the new plaintext from NEWPW env var, hashes with argon2id
(parameters matching crates/manager/src/password.rs: Argon2::default),
and updates users.password_hash for the platform admin. Prints no secrets.
"""
import os
import sqlite3
import sys

import argon2

DB = os.environ.get("BUZ_DB", "example/data/buz.db")
USERNAME = os.environ.get("BUZ_USER", "admin")

pw = os.environ.get("NEWPW", "")
if not pw:
    sys.exit("NEWPW env var is required")

# Argon2::default() in Rust argon2 0.5 = argon2id v19, m=19456, t=2, p=1,
# which is what argon2-cffi's PasswordHasher() produces by default.
ph = argon2.PasswordHasher()
hashed = ph.hash(pw)

conn = sqlite3.connect(DB)
try:
    cur = conn.execute(
        "SELECT id FROM users WHERE username = ? AND is_platform_admin = 1",
        (USERNAME,),
    )
    row = cur.fetchone()
    if row is None:
        sys.exit(f"no platform admin '{USERNAME}' found in {DB}")
    conn.execute(
        "UPDATE users SET password_hash = ? WHERE id = ?",
        (hashed, row[0]),
    )
    conn.commit()
finally:
    conn.close()

print(f"password reset OK for admin '{USERNAME}'")
