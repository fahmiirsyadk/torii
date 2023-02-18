#!/bin/bash

dune exec src/main.exe --cache=enabled &
fswatch -o src/**/*.ml -l 2 | xargs -L1 bash -c \
  "(dune exec src/main.exe --cache=enabled || true) &"