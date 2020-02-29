#!/bin/bash

docker network create --driver bridge devnet

docker run --rm -it \
  --name mariadb \
  --network devnet \
  -e MYSQL_ROOT_PASSWORD=devpassword \
  -p 3306:3306 \
  -d mariadb