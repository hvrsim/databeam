FROM golang:1.25-bookworm AS builder
WORKDIR /app

COPY go.mod go.sum ./
RUN go mod download

COPY cmd ./cmd
COPY internal ./internal
COPY web ./web

RUN CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -trimpath -ldflags="-s -w" -o /out/databeam ./cmd/databeam

FROM debian:bookworm-slim
WORKDIR /app

COPY --from=builder /out/databeam /app/databeam
COPY --from=builder /app/web /app/web

EXPOSE 8080
CMD ["/app/databeam"]
