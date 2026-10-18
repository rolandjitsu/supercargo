FROM rust:1 AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p svc-alpha

FROM gcr.io/distroless/cc-debian12
COPY --from=build /src/target/release/svc-alpha /svc-alpha
ENTRYPOINT ["/svc-alpha"]
