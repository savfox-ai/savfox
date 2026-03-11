ALTER TABLE logs ADD COLUMN session_id TEXT;

CREATE INDEX idx_logs_session_id ON logs(session_id);
