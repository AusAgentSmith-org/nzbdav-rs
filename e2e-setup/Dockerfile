# Test/configure helper image for docker-compose.streaming-clients-e2e.yml.
# Baked-in scripts work around the host/container path mismatch when this
# repo lives in a bind-mounted volume that the docker daemon can't see.

FROM alpine:3.21

RUN apk add --no-cache bash curl python3 jq fuse3 rclone grep

WORKDIR /setup
COPY nzbdav-configure.sh nzbhydra2-configure.sh test-streaming-e2e.sh ./
RUN chmod +x /setup/*.sh
