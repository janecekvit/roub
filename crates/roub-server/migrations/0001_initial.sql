-- roub metadata schema, version 1.
--
-- Object ids are 32-byte BLAKE3 digests (docs/design/object-format.md). Trees and changesets are
-- stored here; blob contents live in the blob store and only their index is here. Everything is
-- scoped by repository, so one repository can never serve another's objects.
--
-- Objects and the ref log are immutable: triggers reject UPDATE, DELETE and TRUNCATE, so neither
-- a bug nor a stray statement can rewrite history. Garbage collection, when it comes, will need
-- its own migration and an explicit, audited path.

CREATE TABLE repositories (
    id   BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

-- Blobs whose content is in the blob store. A row is written only after the content is durable.
CREATE TABLE blobs (
    repository_id BIGINT NOT NULL REFERENCES repositories (id),
    id            BYTEA  NOT NULL CHECK (octet_length(id) = 32),
    size          BIGINT NOT NULL CHECK (size >= 0),
    PRIMARY KEY (repository_id, id)
);

CREATE TABLE trees (
    repository_id BIGINT NOT NULL REFERENCES repositories (id),
    id            BYTEA  NOT NULL CHECK (octet_length(id) = 32),
    content       BYTEA  NOT NULL,
    PRIMARY KEY (repository_id, id)
);

-- The write-ahead log of changesets with their derived root trees.
CREATE TABLE changesets (
    repository_id BIGINT      NOT NULL REFERENCES repositories (id),
    id            BYTEA       NOT NULL CHECK (octet_length(id) = 32),
    content       BYTEA       NOT NULL,
    root_tree     BYTEA       NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (repository_id, id),
    FOREIGN KEY (repository_id, root_tree) REFERENCES trees (repository_id, id)
);

-- One row per ref, kept after a delete (target NULL) so the version never restarts.
CREATE TABLE refs (
    repository_id BIGINT NOT NULL REFERENCES repositories (id),
    name          TEXT   NOT NULL,
    target        BYTEA,
    version       BIGINT NOT NULL CHECK (version > 0),
    PRIMARY KEY (repository_id, name),
    FOREIGN KEY (repository_id, target) REFERENCES changesets (repository_id, id)
);

-- Every move of every ref, in version order. The verifiable ref log is built from these rows.
CREATE TABLE ref_log (
    repository_id BIGINT      NOT NULL REFERENCES repositories (id),
    name          TEXT        NOT NULL,
    version       BIGINT      NOT NULL CHECK (version > 0),
    old_target    BYTEA,
    new_target    BYTEA,
    moved_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (repository_id, name, version)
);

CREATE FUNCTION roub_reject_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'rows of % are immutable', TG_TABLE_NAME;
END
$$;

CREATE FUNCTION roub_check_ref_move() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.repository_id <> OLD.repository_id OR NEW.name <> OLD.name THEN
        RAISE EXCEPTION 'a ref cannot be renamed or moved to another repository';
    END IF;
    IF NEW.version <> OLD.version + 1 THEN
        RAISE EXCEPTION 'ref % must move from version % to %, not %',
            OLD.name, OLD.version, OLD.version + 1, NEW.version;
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER blobs_immutable BEFORE UPDATE OR DELETE ON blobs
    FOR EACH ROW EXECUTE FUNCTION roub_reject_change();
CREATE TRIGGER trees_immutable BEFORE UPDATE OR DELETE ON trees
    FOR EACH ROW EXECUTE FUNCTION roub_reject_change();
CREATE TRIGGER changesets_immutable BEFORE UPDATE OR DELETE ON changesets
    FOR EACH ROW EXECUTE FUNCTION roub_reject_change();
CREATE TRIGGER ref_log_immutable BEFORE UPDATE OR DELETE ON ref_log
    FOR EACH ROW EXECUTE FUNCTION roub_reject_change();
CREATE TRIGGER refs_kept BEFORE DELETE ON refs
    FOR EACH ROW EXECUTE FUNCTION roub_reject_change();
CREATE TRIGGER refs_version_grows BEFORE UPDATE ON refs
    FOR EACH ROW EXECUTE FUNCTION roub_check_ref_move();

CREATE TRIGGER blobs_no_truncate BEFORE TRUNCATE ON blobs
    FOR EACH STATEMENT EXECUTE FUNCTION roub_reject_change();
CREATE TRIGGER trees_no_truncate BEFORE TRUNCATE ON trees
    FOR EACH STATEMENT EXECUTE FUNCTION roub_reject_change();
CREATE TRIGGER changesets_no_truncate BEFORE TRUNCATE ON changesets
    FOR EACH STATEMENT EXECUTE FUNCTION roub_reject_change();
CREATE TRIGGER refs_no_truncate BEFORE TRUNCATE ON refs
    FOR EACH STATEMENT EXECUTE FUNCTION roub_reject_change();
CREATE TRIGGER ref_log_no_truncate BEFORE TRUNCATE ON ref_log
    FOR EACH STATEMENT EXECUTE FUNCTION roub_reject_change();
