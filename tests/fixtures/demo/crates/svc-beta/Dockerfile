FROM rust:1 AS build
ARG PROFILE=release
WORKDIR /src
COPY . .
RUN cargo build --profile "$PROFILE" -p svc-beta

FROM gcr.io/distroless/cc-debian12
COPY --from=build /src/target/release/svc-beta /svc-beta
COPY configs/beta.yaml /etc/beta.yaml
ENTRYPOINT ["/svc-beta"]
