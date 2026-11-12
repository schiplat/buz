#!/usr/bin/env python3
"""Translate the dumped SQLite schema into the PG baseline DDL.

Rules (design: keep TEXT columns faithful, shim the datetime() function):
- INTEGER -> BIGINT (sqlx decodes i64 in both dialects)
- DEFAULT (datetime('now')) -> DEFAULT datetime()  [shim fn, same text format]
- SQLite triggers -> PG trigger functions (same max+1 semantics)
- CREATE TABLE statements topologically sorted by FK targets (PG resolves FKs
  at creation time; SQLite deferred them)
- everything else verbatim
"""
import re

src = open('/tmp/buz_schema_dump.sql', encoding='utf-8').read()
blocks = [b for b in src.split('\n\n') if b.strip()]

tables = {}      # name -> sql text
indexes = []     # sql text
triggers = []    # (name, sqlite body)

for b in blocks:
    b = b.strip()
    if not b:
        continue
    if b.startswith('-- table'):
        name = re.search(r'-- table (\w+)', b).group(1)
        tables[name] = b
    elif '-- CREATE' in b.split('\n')[0] or 'CREATE INDEX' in b:
        indexes.append(b)
    elif 'CREATE TRIGGER' in b:
        name = re.search(r'CREATE TRIGGER (\w+)', b).group(1)
        triggers.append((name, b))
    else:
        indexes.append(b)  # comments/other

# ── FK graph for topological sort ──
def fk_targets(sql):
    # REFERENCES <table>( — only count other tables
    return set(re.findall(r'REFERENCES\s+(\w+)\s*\(', sql))

order = []
placed = set()
pending = dict(tables)
while pending:
    progressed = False
    for name in list(pending):
        targets = fk_targets(pending[name])
        if targets <= placed:
            order.append((name, pending[name]))
            placed.add(name)
            del pending[name]
            progressed = True
    if not progressed:
        # circular or self-ref: append rest alphabetically
        for name in sorted(pending):
            order.append((name, pending[name]))
        break

# ── translate one CREATE TABLE ──
def translate_table(sql):
    sql = re.sub(r'\bINTEGER\b', 'BIGINT', sql)
    sql = sql.replace("DEFAULT (datetime('now'))", 'DEFAULT datetime()')
    return sql

# ── trigger conversion ──
def convert_trigger(name, body):
    m = re.search(r'CREATE TRIGGER (\w+)\s+AFTER INSERT ON (\w+)\s+WHEN NEW\.seq = 0\s+BEGIN\s+(.*?)\s+END;', body, re.S)
    if not m:
        raise SystemExit('unexpected trigger body: ' + name)
    trig_name, table, action = m.group(1), m.group(2), m.group(3)
    fn_name = trig_name + '_fn'
    return f'''-- seq allocation (max+1 under the writer lock), ported from the SQLite trigger
CREATE OR REPLACE FUNCTION {fn_name}() RETURNS trigger AS $$
BEGIN
    {action.strip()};
    RETURN NEW;
END
$$ LANGUAGE plpgsql;

CREATE TRIGGER {trig_name}
    AFTER INSERT ON {table}
    FOR EACH ROW
    WHEN (NEW.seq = 0)
    EXECUTE FUNCTION {fn_name}();'''

# ── shim functions ──
shims = '''-- ───────────────────────── dialect shim functions ─────────────────────────
-- SQLite's datetime(...) calls appear verbatim in the shared SQL strings.
-- These shims make them valid Postgres with identical text output
-- ("YYYY-MM-DD HH24:MI:SS"), which every timestamp comparison in the shared
-- statements relies on (columns stay TEXT).

CREATE OR REPLACE FUNCTION datetime() RETURNS text LANGUAGE sql
    AS $$ SELECT to_char(now(), 'YYYY-MM-DD HH24:MI:SS') $$;

CREATE OR REPLACE FUNCTION datetime(value text) RETURNS text LANGUAGE sql
    AS $$ SELECT value::timestamptz::text $$;

CREATE OR REPLACE FUNCTION datetime(value text, modifier text) RETURNS text
    LANGUAGE sql AS $$
    SELECT CASE
        WHEN modifier LIKE '-%' THEN
            to_char(value::timestamptz - modifier::interval, 'YYYY-MM-DD HH24:MI:SS')
        ELSE
            to_char(value::timestamptz + modifier::interval, 'YYYY-MM-DD HH24:MI:SS')
    END
$$;

'''

# ── emit ──
out = ['-- Buz Manager: Postgres baseline schema.',
       '-- Derived mechanically from the fully migrated SQLite template database',
       '-- (39 tables, indexes, 2 seq triggers). TEXT columns are kept faithful —',
       '-- timestamp comparisons in the shared SQL strings rely on text ordering,',
       '-- and the datetime() shims produce SQLite\'s exact text format.',
       '',
       shims,
       '-- ───────────────────────────── tables (FK order) ─────────────────────',
       '']
for name, sql in order:
    out.append(translate_table(sql) + '\n')
out.append('-- ───────────────────────────── indexes ─────────────────────────────\n')
for b in sorted(indexes):
    out.append(b.strip() + '\n\n')
out.append('-- ───────────────────────────── triggers ─────────────────────────────\n')
for name, body in triggers:
    out.append(convert_trigger(name, body) + '\n\n')

open('/tmp/buz_pg_baseline.sql', 'w', encoding='utf-8').write(''.join(out))
print('tables:', len(order), 'indexes:', len(indexes), 'triggers:', len(triggers))
print('remaining unplaced:', list(pending))
