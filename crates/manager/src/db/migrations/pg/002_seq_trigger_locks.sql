-- Sequence allocation: `MAX(seq) + 1` is a read-then-write decision. SQLite's
-- write lock serialises it; Postgres does not, so the AFTER INSERT triggers
-- take a transaction-scoped advisory lock before reading the maximum. The
-- following UPDATE is a new statement and therefore sees the previous writer's
-- committed row under the default READ COMMITTED isolation level.
--
-- Keys are the ASCII bytes of "buzseqj1" / "buzseqa1"; keep them distinct from
-- the manager instance lock (0x62757a6d67723031) and the setup lock
-- (0x62757a7365747570 in store::locks).
--
-- Version 1 (the baseline) carries the unlocked bodies. Deployed databases
-- apply this migration once; fresh databases run it right after the baseline.

CREATE OR REPLACE FUNCTION trg_jobs_insert_seq_fn() RETURNS trigger AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(7094711424033843761); -- "buzseqj1"
    UPDATE jobs SET seq = (SELECT COALESCE(MAX(seq), 0) + 1 FROM jobs)
             WHERE id = NEW.id;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION trg_audit_logs_insert_seq_fn() RETURNS trigger AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(7094711424033841457); -- "buzseqa1"
    UPDATE audit_logs SET seq = (SELECT COALESCE(MAX(seq), 0) + 1 FROM audit_logs)
             WHERE id = NEW.id;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
