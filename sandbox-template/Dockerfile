FROM ubuntu:24.04

# Required: Blaxel sandbox API
COPY --from=ghcr.io/blaxel-ai/sandbox:latest /sandbox-api /usr/local/bin/sandbox-api

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl git netcat-openbsd openssh-client python3 \
    && rm -rf /var/lib/apt/lists/*

COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENTRYPOINT ["/entrypoint.sh"]
