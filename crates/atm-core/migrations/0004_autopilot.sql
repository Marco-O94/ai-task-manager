-- Round 2026-10-01: autopilota per progetto (coda → avvio → verifica → correzione → merge).

ALTER TABLE projects ADD COLUMN autopilot INTEGER NOT NULL DEFAULT 0 CHECK (autopilot IN (0, 1));
ALTER TABLE projects ADD COLUMN autopilot_merge INTEGER NOT NULL DEFAULT 0
    CHECK (autopilot_merge IN (0, 1));
-- Eseguito con /bin/sh -c nel worktree dopo un turno completato; NULL = nessuna verifica.
ALTER TABLE projects ADD COLUMN verify_command TEXT
    CHECK (verify_command IS NULL OR length(verify_command) BETWEEN 1 AND 1000);
ALTER TABLE projects ADD COLUMN verify_timeout_secs INTEGER NOT NULL DEFAULT 600
    CHECK (verify_timeout_secs BETWEEN 10 AND 3600);
ALTER TABLE projects ADD COLUMN autopilot_max_fixes INTEGER NOT NULL DEFAULT 2
    CHECK (autopilot_max_fixes BETWEEN 0 AND 5);

-- Il task è affidato all'autopilota.
ALTER TABLE tasks ADD COLUMN auto INTEGER NOT NULL DEFAULT 0 CHECK (auto IN (0, 1));
-- Una sola dipendenza: il task parte dopo che `after_id` è Fatto (stesso progetto, validato
-- dal core). Eliminare la dipendenza la toglie.
ALTER TABLE tasks ADD COLUMN after_id TEXT REFERENCES tasks(id) ON DELETE SET NULL;
CREATE INDEX tasks_after ON tasks(after_id);
-- Attempt il cui agente ha affidato il task all'autopilota (`start_task` in coda, sotto task
-- ereditato); NULL = l'utente. Testo semplice come `started_by_attempt`: l'autopilota lo avvia
-- come l'avrebbe avviato quell'agente, e la catena resta a profondità 2.
ALTER TABLE tasks ADD COLUMN auto_by TEXT;

-- Ultima verifica dell'attempt: NULL = mai verificato.
ALTER TABLE attempts ADD COLUMN verify_state TEXT
    CHECK (verify_state IN ('running', 'passed', 'failed', 'error'));
ALTER TABLE attempts ADD COLUMN verify_head TEXT;  -- commit verificato (HEAD alla partenza)
-- Rimandi all'agente dopo una verifica fallita o un conflitto.
ALTER TABLE attempts ADD COLUMN verify_fixes INTEGER NOT NULL DEFAULT 0 CHECK (verify_fixes >= 0);
ALTER TABLE attempts ADD COLUMN verify_summary TEXT;  -- coda dell'output, al massimo 8 KiB
-- Prompt di una correzione in attesa di uno slot libero (o della fine della pausa): lo
-- scheduler la manda prima di avviare altri task della coda.
ALTER TABLE attempts ADD COLUMN verify_pending TEXT;
