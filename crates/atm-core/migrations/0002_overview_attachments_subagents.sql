-- Round feature 2026-09-29: descrizione del progetto, limite e modello dei sub-agent, allegati.
-- ADD COLUMN controlla i CHECK sulle righe esistenti: i default li soddisfano.

ALTER TABLE projects ADD COLUMN description TEXT NOT NULL DEFAULT ''
  CHECK (length(description) <= 10000);

ALTER TABLE attempts ADD COLUMN subagent_model TEXT;       -- alias di MODEL_ALIASES; NULL = default del CLI
ALTER TABLE attempts ADD COLUMN max_subagents INTEGER
  CHECK (max_subagents IS NULL OR max_subagents BETWEEN 0 AND 10);   -- NULL = nessun limite
ALTER TABLE attempts ADD COLUMN subagents_used INTEGER NOT NULL DEFAULT 0
  CHECK (subagents_used >= 0);

CREATE TABLE task_attachments (                          -- file in data_dir/attachments/, mai nel worktree
  id         TEXT PRIMARY KEY,
  task_id    TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  name       TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255),
  size       INTEGER NOT NULL CHECK (size >= 0),
  created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX task_attachments_task ON task_attachments(task_id, created_at);
