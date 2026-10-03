-- Semantic guards for `works` (SEMANTIC_MODEL.md B2 + B3).
-- SEMANTIC_MODEL says "CHECK" for B2, but SQLite cannot ALTER TABLE ... ADD
-- CHECK, and rebuilding `works` would disturb the FTS external-content
-- triggers and indexes - so triggers are the deliberate mechanism.
-- B2: an identified work has provenance - work_identity IS NOT NULL implies
-- identification_source IN ('api', 'filename', 'manual'), never 'failed'.
-- B3: a work in a personal collection is never identified.

CREATE TRIGGER works_b2_insert BEFORE INSERT ON works
    WHEN NEW.work_identity IS NOT NULL
        AND (NEW.identification_source IS NULL
             OR NEW.identification_source NOT IN ('api', 'filename', 'manual'))
    BEGIN
        SELECT RAISE(ABORT, 'B2: an identified work needs identification_source in (api, filename, manual)');
    END;

CREATE TRIGGER works_b2_update BEFORE UPDATE ON works
    WHEN NEW.work_identity IS NOT NULL
        AND (NEW.identification_source IS NULL
             OR NEW.identification_source NOT IN ('api', 'filename', 'manual'))
    BEGIN
        SELECT RAISE(ABORT, 'B2: an identified work needs identification_source in (api, filename, manual)');
    END;

CREATE TRIGGER works_b3_insert BEFORE INSERT ON works
    WHEN NEW.work_identity IS NOT NULL
        AND EXISTS (
            SELECT 1 FROM collections
            WHERE id = NEW.collection_id AND type = 'personal'
        )
    BEGIN
        SELECT RAISE(ABORT, 'B3: a work in a personal collection is never identified');
    END;

CREATE TRIGGER works_b3_update BEFORE UPDATE ON works
    WHEN NEW.work_identity IS NOT NULL
        AND EXISTS (
            SELECT 1 FROM collections
            WHERE id = NEW.collection_id AND type = 'personal'
        )
    BEGIN
        SELECT RAISE(ABORT, 'B3: a work in a personal collection is never identified');
    END;
