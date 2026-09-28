CREATE TABLE settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL CHECK (json_valid(value))
) STRICT;

CREATE TABLE projects (
  id                      TEXT PRIMARY KEY,
  name                    TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
  repo_path               TEXT NOT NULL UNIQUE,          -- toplevel canonico del checkout principale
  default_target_branch   TEXT NOT NULL,
  default_permission_mode TEXT NOT NULL DEFAULT 'acceptEdits'
                          CHECK (default_permission_mode IN ('default','acceptEdits','bypassPermissions')),
  default_model           TEXT,
  config_policy           TEXT NOT NULL DEFAULT 'isolated' CHECK (config_policy IN ('isolated','trusted')),
  trusted_fingerprint     TEXT,                          -- sha256 hex (§8.9); NULL = mai approvato
  allow_bypass            INTEGER NOT NULL DEFAULT 0 CHECK (allow_bypass IN (0,1)),
  created_at              INTEGER NOT NULL,
  updated_at              INTEGER NOT NULL
) STRICT;

CREATE TABLE tasks (
  id          TEXT PRIMARY KEY,
  project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  title       TEXT NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
  description TEXT NOT NULL DEFAULT '' CHECK (length(description) <= 100000),
  status      TEXT NOT NULL DEFAULT 'todo'
              CHECK (status IN ('todo','inprogress','inreview','done','cancelled')),
  position    REAL NOT NULL,                             -- ordine nella colonna (gap 1024, midpoint)
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
) STRICT;
CREATE INDEX tasks_board ON tasks(project_id, status, position);

CREATE TABLE attempts (
  id              TEXT PRIMARY KEY,
  task_id         TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  state           TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active','merged','discarded')),
  branch          TEXT NOT NULL,                         -- atm/<8hex>-<slug>, immutabile
  target_branch   TEXT NOT NULL,
  base_commit     TEXT NOT NULL,                         -- tip del target alla creazione
  worktree_path   TEXT NOT NULL UNIQUE,                  -- canonico, immutabile
  worktree_state  TEXT NOT NULL DEFAULT 'present' CHECK (worktree_state IN ('present','removed','missing')),
  session_id      TEXT NOT NULL UNIQUE,                  -- UUID assegnato dall'app
  session_started INTEGER NOT NULL DEFAULT 0 CHECK (session_started IN (0,1)),
  permission_mode TEXT NOT NULL CHECK (permission_mode IN ('default','acceptEdits','bypassPermissions')),
  model           TEXT,
  effort          TEXT CHECK (effort IS NULL OR effort IN ('low','medium','high','xhigh','max')),
  allow_rules     TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(allow_rules)),  -- "consenti sempre" (§7.8)
  merge_commit    TEXT,
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL,
  closed_at       INTEGER
) STRICT;
CREATE UNIQUE INDEX attempts_one_active ON attempts(task_id) WHERE state = 'active';
CREATE INDEX attempts_task ON attempts(task_id, created_at);

CREATE TABLE processes (                                 -- 1 riga = 1 turno claude
  id                TEXT PRIMARY KEY,
  attempt_id        TEXT NOT NULL REFERENCES attempts(id) ON DELETE CASCADE,
  seq               INTEGER NOT NULL,
  prompt            TEXT NOT NULL,
  permission_mode   TEXT NOT NULL,
  session_id        TEXT NOT NULL,
  resumed           INTEGER NOT NULL CHECK (resumed IN (0,1)),
  status            TEXT NOT NULL DEFAULT 'running'
                    CHECK (status IN ('running','completed','failed','killed')),
  stop_reason       TEXT CHECK (stop_reason IS NULL OR stop_reason IN
                    ('user_stop','app_shutdown','app_restart','spawn_error','init_timeout',
                     'exit_timeout','crash','auth_failure','usage_limit')),
  error             TEXT,
  cli_version       TEXT,
  argv_json         TEXT NOT NULL CHECK (json_valid(argv_json)),   -- mai l'env
  pid               INTEGER,                              -- = pgid (process_group(0))
  app_instance_id   TEXT NOT NULL,                        -- UUID dell'avvio dell'app
  exit_code         INTEGER,
  result_subtype    TEXT,
  is_error          INTEGER CHECK (is_error IS NULL OR is_error IN (0,1)),
  cost_usd_estimate REAL,                                 -- stima client-side [F]
  num_turns         INTEGER,
  duration_ms       INTEGER,
  head_before       TEXT,
  head_after        TEXT,                                 -- dopo l'auto-commit
  started_at        INTEGER NOT NULL,
  finished_at       INTEGER,
  UNIQUE (attempt_id, seq)
) STRICT;
CREATE UNIQUE INDEX processes_one_running ON processes(attempt_id) WHERE status = 'running';
CREATE INDEX processes_running ON processes(status) WHERE status = 'running';

CREATE TABLE entries (                                   -- transcript normalizzato
  attempt_id TEXT NOT NULL REFERENCES attempts(id) ON DELETE CASCADE,
  idx        INTEGER NOT NULL,                            -- monotono per attempt
  rev        INTEGER NOT NULL,                            -- +1 a ogni upsert
  process_id TEXT NOT NULL,
  kind       TEXT NOT NULL,
  payload    TEXT NOT NULL CHECK (json_valid(payload)),  -- atm_types::Entry
  ts         INTEGER NOT NULL,
  PRIMARY KEY (attempt_id, idx)
) STRICT, WITHOUT ROWID;
