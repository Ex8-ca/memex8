#!/bin/bash
cd /home/marc/memex8
docker compose up -d --force-recreate --no-deps memex8
echo "EXIT:$?"
