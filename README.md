# api.view.art

Welcome! This project serves as the backbone for our open-source protocol, designed to allow multiple clients to read and record playlist data and manage channel ownership seamlessly. Inspired by robust communication protocols like Warpcast :: Farcaster and Gmail :: SMTP, our backend provides a shared state that can be accessed by various clients.

Our vision is to create an ecosystem where multiple clients can interact with a shared backend, ensuring data consistency and integrity across all platforms. By open-sourcing the backend, we aim to foster collaboration, innovation, and transparency within the developer community, while keeping our client code private to focus on the continuous improvement and evolution of our product.

### Key Features

    - Open-Source Protocol: The backend is publicly available, encouraging contributions and enhancements from the community.
    - Rust-Based: Leveraging the power and performance of Rust, ensuring a reliable and efficient backend service.
    - Multi-Client Support: Designed to allow multiple clients to interact with the shared state, promoting a collaborative ecosystem.
    - Data Integrity: Ensures consistency and accuracy of playlist data and channel ownership across all clients.

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
