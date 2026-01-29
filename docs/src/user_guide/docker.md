# Docker

Eidetica provides official container images for running the database server in containerized environments. Images are built using Nix, so they are minimal in size but lack any extra tools (curl, wget, etc).

There is a maintained Dockerfile in the repo but it is not published.

## Container Registries

Official images are available from:

- **GitHub Container Registry**: `ghcr.io/arcuru/eidetica`
- **Docker Hub**: `arcuru/eidetica`

Both registries contain identical images with multi-architecture support (amd64 and arm64).

## Available Tags

| Tag      | Description                                       |
| -------- | ------------------------------------------------- |
| `dev`    | (Recommended) Development builds from main branch |
| `latest` | Latest stable release                             |
| `X.Y.Z`  | Specific version (e.g., `0.1.0`)                  |
| `X.Y`    | Latest patch release for minor version            |

## Configuration

### Environment Variables

| Variable                | Description                                             | Default                  |
| ----------------------- | ------------------------------------------------------- | ------------------------ |
| `EIDETICA_PORT`         | Port for the HTTP server                                | `3000`                   |
| `EIDETICA_HOST`         | Bind address                                            | `0.0.0.0` (in container) |
| `EIDETICA_BACKEND`      | Storage backend (`sqlite`, `postgres`, `inmemory`)      | `sqlite`                 |
| `EIDETICA_DATA_DIR`     | Directory for database and data files                   | `/config` (in container) |
| `EIDETICA_POSTGRES_URL` | PostgreSQL connection URL (when using postgres backend) | -                        |

### Data Storage

The container stores data in `/config` by default. When using Docker volumes, ensure the directory has proper ownership (UID 1000, the `eidetica` user in the container).

### Health Checks

The container includes a built-in health check that verifies the server is responding. You can also run it manually:

```bash
docker exec <container> eidetica health
```

## Quick Start

Pull and run the latest stable image:

```bash
docker run -p 3000:3000 ghcr.io/arcuru/eidetica:latest
```

Or from Docker Hub:

```bash
docker run -p 3000:3000 arcuru/eidetica:latest
```

## Docker Compose

For production deployments, Docker Compose provides a convenient way to manage Eidetica alongside other services.

### Basic Configuration

Create a `docker-compose.yml` file:

```yaml
services:
  eidetica:
    image: ghcr.io/arcuru/eidetica:latest
    ports:
      - "3000:3000"
    volumes:
      - eidetica-data:/config
    restart: unless-stopped

volumes:
  eidetica-data:
```

> **Note**: When using bind mounts instead of named volumes, ensure the host directory is owned by UID 1000:
>
> ```bash
> sudo chown -R 1000:1000 /path/to/config
> ```

Start the service:

```bash
docker compose up -d
```

### With PostgreSQL Backend

For production use with PostgreSQL as the storage backend:

```yaml
services:
  eidetica:
    image: ghcr.io/arcuru/eidetica:latest
    ports:
      - "3000:3000"
    environment:
      EIDETICA_BACKEND: postgres
      EIDETICA_POSTGRES_URL: postgres://eidetica:secret@postgres:5432/eidetica
    depends_on:
      postgres:
        condition: service_healthy
    restart: unless-stopped

  postgres:
    image: postgres:16-alpine
    environment:
      POSTGRES_USER: eidetica
      POSTGRES_PASSWORD: secret
      POSTGRES_DB: eidetica
    volumes:
      - postgres-data:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U eidetica -d eidetica"]
      interval: 5s
      timeout: 5s
      retries: 5
    restart: unless-stopped

volumes:
  postgres-data:
```

## Building Images Locally

The published image is built with Nix. The justfile contains scripts for building the image locally.

There is also a Dockerfile-based build available and maintained.

Build with:

```bash
just container nix
just container docker
```
