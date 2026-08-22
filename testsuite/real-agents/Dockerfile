# syntax=docker/dockerfile:1.7
#
# The opt-in real-agent layer, built on top of the test image.
#
# It exists for one reason: butai reads agent status by matching strings the
# agent CLIs draw, and there is no protocol to version. The doubles in
# `testsuite/fakeagents/` reproduce those strings, and this layer is how we find
# out when upstream changes them.
#
#   ./testsuite/run.sh standard --real-agents
#
# An install that fails is not fatal — `test_41_agents_real.py` skips any agent
# whose binary is absent — but it is logged loudly, because a silently missing
# agent means silently missing coverage.

ARG BASE=butai-testsuite:latest
FROM ${BASE}

USER root

RUN apt-get update && apt-get install -y --no-install-recommends \
        nodejs \
        npm \
        python3-pip \
        python3-venv \
    && rm -rf /var/lib/apt/lists/*

RUN for pkg in @anthropic-ai/claude-code @openai/codex @google/gemini-cli; do \
        npm install -g --no-fund --no-audit "$pkg" \
            || echo "WARNING: could not install $pkg — its tests will report SKIP"; \
    done

# aider is a large Python install; kept in its own venv so it cannot disturb the
# system interpreter the suite itself runs on.
RUN python3 -m venv /opt/aider \
    && /opt/aider/bin/pip install --no-cache-dir --quiet aider-chat \
    && ln -sf /opt/aider/bin/aider /usr/local/bin/aider \
    || echo "WARNING: could not install aider — its tests will report SKIP"

USER butai

# Agents write config and history under HOME; make sure they can.
RUN mkdir -p /home/butai/.config /home/butai/.cache
