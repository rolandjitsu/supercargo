# Tool image for GitLab CI jobs: supercargo plus the cargo and git it drives
# (`plan` runs `cargo metadata`; hashing reads git blob OIDs). No ENTRYPOINT -
# GitLab job images need the default shell.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p supercargo

FROM rust:1-alpine
RUN apk add --no-cache git
COPY --from=build /src/target/release/supercargo /usr/local/bin/supercargo
