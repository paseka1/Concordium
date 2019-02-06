#!/bin/bash
if [ -z "$1" ]
then
    echo "Please specify external IP as first argument"
    exit 1
fi

if [ -z "$2" ]
then
    echo "Please specify number of nodes as second argument"
    exit 1
fi

EXTERNAL_IP=$1
NODE_COUNT=$2
docker run -d --name=bootstrapper -p 8888:8888 concordium/test/bootstrapper:latest
sleep 5
for i in `seq 1 $NODE_COUNT`;
do
    PORT=$((8889+$i))
    docker run -d --name=nodetest$i -p $PORT:8888 -e "EXTERNAL_PORT=$PORT" -e "BOOTSTRAP_NODE=$EXTERNAL_IP:8888" -e "BAKER_ID=$(($i-1))" -e "NUM_BAKERS=$NODE_COUNT" concordium/test/node:latest 
    ps aux | grep p2p_client-cli
done
TESTRUNNER_PORT=$((8889+$NODE_COUNT+1))
docker run -d --name=testrunner -p $TESTRUNNER_PORT:8888 -e "EXTERNAL_PORT=$TESTRUNNER_PORT" -e "BOOTSTRAP_NODE=$EXTERNAL_IP:8888" concordium/test/testrunner:latest
