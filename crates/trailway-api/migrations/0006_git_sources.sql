-- A service runs either an image or a build of a public git repository.
ALTER TABLE services ALTER COLUMN image DROP NOT NULL;
ALTER TABLE services ADD COLUMN git_url text;
ALTER TABLE services ADD COLUMN git_branch text;
ALTER TABLE services ADD CONSTRAINT services_source_check CHECK (
    (image IS NOT NULL AND git_url IS NULL AND git_branch IS NULL)
    OR (image IS NULL AND git_url IS NOT NULL AND git_branch IS NOT NULL)
);

ALTER TABLE deployments ADD COLUMN source jsonb;
ALTER TABLE deployments ADD COLUMN commit_sha text;
