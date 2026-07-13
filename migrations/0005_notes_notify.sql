-- Push-based note fan-out (Issue 8): NOTIFY on every encrypted note
-- insert so the API can LISTEN instead of polling. The payload is the
-- note id, but listeners treat a notification only as a wake-up and read
-- rows > their cursor — notifications are allowed to coalesce or drop
-- (the API keeps a polling safety net).
CREATE OR REPLACE FUNCTION notify_attesta_note() RETURNS trigger AS $$
BEGIN
    PERFORM pg_notify('attesta_notes', NEW.id::text);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER encrypted_notes_notify
    AFTER INSERT ON encrypted_notes
    FOR EACH ROW EXECUTE FUNCTION notify_attesta_note();
