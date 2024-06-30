# view.art - backend

## Development

You can use a pre-commit hook to ensure that there are no formatting issues and no clippy issues in the code. See the file [`.githooks/pre-commit`](.githooks/pre-commit) for details and installation instructions.

## Testing

Requires [nextest](https://nexte.st/) and [docker-compose](https://docs.docker.com/compose/).

Note: this flushes the redis database before running the tests.

1. Run required services: `docker compose up`.
2. Run the tests: `cargo nextest run`.
3. Stop the services and remove volumes: `docker compose down -v`.

## Running

1. Create and complete a `.env` file using `.env.example` as a template.
2. Run required services: `docker compose up`.
3. Run the backend: `cargo run`.

Notes:

 - Running `RUST_LOG=debug cargo run` runs the backend with debug traces.

## Configuration

Configuration is possible via a `.env` file, CLI args and environment variables.
