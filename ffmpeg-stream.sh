#!/bin/sh
ffmpeg \
  -loglevel level+info \
  -r 60 \
  -f x11grab \
  -i :0.0 \
  -c:v h264_nvenc \
  -b:v 12M \
  -preset p1 \
  -tune ll \
  -rc cbr \
  -bufsize 24M \
  -rc-lookahead 0 \
  -bf 0 \
  -g 120 \
  -pix_fmt yuv420p \
  -f mpegts 'srt://127.0.0.1:8083?mode=caller&latency=50'
