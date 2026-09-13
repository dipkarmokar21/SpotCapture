CXX ?= g++
CXXFLAGS ?= -O2 -std=c++17 -Wall -Wextra -Wpedantic
TK_FLAGS := $(shell pkg-config --cflags --libs tk)
ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/..)

.PHONY: all check clean
all: $(ROOT)/build/spotcapture-ui

$(ROOT)/build/spotcapture-ui: $(ROOT)/ui/main.cpp $(ROOT)/ui/layout.hpp
	mkdir -p $(ROOT)/build
	$(CXX) $(CXXFLAGS) $(ROOT)/ui/main.cpp -o $@.tmp $(TK_FLAGS)
	mv -f $@.tmp $@

check: all
	$(ROOT)/build/spotcapture-ui --check

clean:
	$(RM) $(ROOT)/build/spotcapture-ui $(ROOT)/build/spotcapture-ui.tmp
