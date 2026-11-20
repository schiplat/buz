-- v7: normalise team bindings (SQLite v8 mirrors this).
--
-- `team_id` becomes the only stored identity: the legacy `owner_group` slug
-- is backfilled once, then dropped; read paths derive the slug by joining
-- `teams`. Secrets rename `team_shared` to the accurate `share_with_team` and
-- move name uniqueness from global to per team; hosts gain per-owner
-- uniqueness (team rows, or the platform tier when team_id is NULL/'').

-- Phase 1 databases had only free-form owner_group slugs: materialise the
-- teams the old startup bootstrap used to create, before the columns go away.
INSERT INTO teams (id, name, slug)
SELECT 'legacy-' || g.owner_group, g.owner_group, g.owner_group
  FROM (
      SELECT DISTINCT owner_group FROM pipelines WHERE owner_group != ''
      UNION
      SELECT DISTINCT owner_group FROM secrets WHERE owner_group != ''
  ) AS g
 WHERE NOT EXISTS (SELECT 1 FROM teams t WHERE t.slug = g.owner_group)
ON CONFLICT DO NOTHING;

-- Backfill from the legacy slug before it disappears. Unknown teams are left
-- for `buz-manager check-data`; the default tier is a fallback when present.
UPDATE pipelines p
   SET team_id = t.id
  FROM teams t
 WHERE (p.team_id IS NULL OR p.team_id = '')
   AND p.owner_group != ''
   AND t.slug = p.owner_group;

UPDATE secrets s
   SET team_id = t.id
  FROM teams t
 WHERE (s.team_id IS NULL OR s.team_id = '')
   AND s.owner_group != ''
   AND t.slug = s.owner_group;

UPDATE pipelines
   SET team_id = (SELECT id FROM teams WHERE slug = 'default')
 WHERE (team_id IS NULL OR team_id = '')
   AND EXISTS (SELECT 1 FROM teams WHERE slug = 'default');

UPDATE secrets
   SET team_id = (SELECT id FROM teams WHERE slug = 'default')
 WHERE (team_id IS NULL OR team_id = '')
   AND EXISTS (SELECT 1 FROM teams WHERE slug = 'default');

ALTER TABLE pipelines DROP COLUMN owner_group;
ALTER TABLE secrets DROP COLUMN owner_group;

ALTER TABLE secrets RENAME COLUMN team_shared TO share_with_team;

-- Name scoping: secrets and hosts are unique per owner, not globally.
ALTER TABLE secrets DROP CONSTRAINT IF EXISTS secrets_name_key;
ALTER TABLE secrets
  ADD CONSTRAINT secrets_team_id_name_key UNIQUE (team_id, name);

ALTER TABLE hosts DROP CONSTRAINT IF EXISTS hosts_name_key;
CREATE UNIQUE INDEX IF NOT EXISTS hosts_team_name_key
    ON hosts (team_id, name)
 WHERE team_id IS NOT NULL AND team_id != '';
CREATE UNIQUE INDEX IF NOT EXISTS hosts_platform_name_key
    ON hosts (name)
 WHERE team_id IS NULL OR team_id = '';
