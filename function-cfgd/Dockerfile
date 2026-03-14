FROM golang:1.23 AS builder
WORKDIR /fn
COPY go.mod go.sum ./
RUN go mod download
COPY . .
RUN CGO_ENABLED=0 go build -o /function .

FROM gcr.io/distroless/static:nonroot
COPY --from=builder /function /function
EXPOSE 9443
USER nonroot:nonroot
ENTRYPOINT ["/function"]
