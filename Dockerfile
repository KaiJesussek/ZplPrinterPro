# Binary is built on the CI runner (see .gitea/workflows/build-docker.yaml);
# this image only carries it.
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/*
COPY release/zpl-server-linux-x64 /zpl-server
EXPOSE 9100 8080
ENV ZPL_ARCHIVE_DIR=/zpl-archive
ENTRYPOINT ["/zpl-server"]
