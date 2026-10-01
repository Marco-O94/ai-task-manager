-- Round 2026-09-30: sotto task e strumenti board per gli agenti.
-- Un solo livello di sotto task (validato dal core); eliminare il padre elimina i figli.

ALTER TABLE tasks ADD COLUMN parent_id TEXT REFERENCES tasks(id) ON DELETE CASCADE;  -- NULL = task di primo livello
CREATE INDEX tasks_parent ON tasks(parent_id);

-- Attempt il cui agente ha avviato questo con `start_task`; NULL = avviato dall'utente.
-- Testo semplice, niente FK: eliminare l'attempt che l'ha avviato non rompe la riga.
ALTER TABLE attempts ADD COLUMN started_by_attempt TEXT;
