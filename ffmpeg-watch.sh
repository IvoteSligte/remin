#!/bin/sh

ffplay -fflags nobuffer -flags low_delay 'srt://0.0.0.0:8083?mode=listener&latency=50'
