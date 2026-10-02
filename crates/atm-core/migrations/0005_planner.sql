-- Round 2026-10-02: pianificazione con un agente («Pianifica con un agente»).

-- Un task 'plan' è il pianificatore: nascosto ovunque (board, strumenti board, autopilota),
-- eliminato con il progetto. Il suo prompt è la descrizione.
ALTER TABLE tasks ADD COLUMN kind TEXT NOT NULL DEFAULT 'task' CHECK (kind IN ('task', 'plan'));
-- Il task 'plan' che ha creato questo task; eliminare il piano lo toglie.
ALTER TABLE tasks ADD COLUMN planned_by TEXT REFERENCES tasks(id) ON DELETE SET NULL;
CREATE INDEX tasks_planned_by ON tasks(planned_by);
-- Stato del piano (solo i task 'plan'): NULL per ogni altro task.
ALTER TABLE tasks ADD COLUMN plan_state TEXT
    CHECK (plan_state IN ('running', 'awaiting', 'started', 'dismissed', 'failed'));
-- Un solo piano attivo (in corso o in attesa di conferma) per progetto.
CREATE UNIQUE INDEX tasks_one_active_plan ON tasks(project_id)
    WHERE kind = 'plan' AND plan_state IN ('running', 'awaiting');
-- Da avviare dallo scheduler anche senza autopilota (task creati da un piano confermato);
-- azzerato quando il task parte.
ALTER TABLE tasks ADD COLUMN launch INTEGER NOT NULL DEFAULT 0 CHECK (launch IN (0, 1));
