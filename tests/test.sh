#!/usr/bin/env bash
set -e

cd "$(dirname "$0")"

if [ "$1" = "up" ]; then
    # Start the database
    docker compose --file ./tests/docker-compose.yml up -d --remove-orphans --wait

elif [ "$1" = "down" ]; then
    # Stop the database
    docker compose --file ./tests/docker-compose.yml down
else
    echo "Usage: $0 [up|down]"
fi

cd -
