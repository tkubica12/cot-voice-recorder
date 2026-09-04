# syntax=docker/dockerfile:1

# ---------------------------------------------------------------------------
# Builder: resolve and install dependencies into a self-contained virtualenv.
# ---------------------------------------------------------------------------
FROM python:3.13-slim AS builder

# Optional: override the package index (e.g. a mirror/proxy). Defaults to PyPI.
ARG UV_DEFAULT_INDEX=""
ARG PIP_INDEX_URL=""
ENV UV_DEFAULT_INDEX=${UV_DEFAULT_INDEX} \
    PIP_INDEX_URL=${PIP_INDEX_URL} \
    UV_LINK_MODE=copy \
    UV_PYTHON_DOWNLOADS=never \
    PIP_DISABLE_PIP_VERSION_CHECK=1

RUN pip install --no-cache-dir "uv>=0.5,<1.0"

WORKDIR /app

# Install dependencies first (better layer caching) using the committed lock.
COPY pyproject.toml uv.lock README.md ./
COPY src ./src
RUN uv sync --frozen --no-dev

# ---------------------------------------------------------------------------
# Runtime: minimal, non-root image with only the venv and source.
# ---------------------------------------------------------------------------
FROM python:3.13-slim AS runtime

ENV PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    PATH="/app/.venv/bin:$PATH" \
    VR_ENVIRONMENT=production

# Create an unprivileged user to run the container.
RUN groupadd --system app && useradd --system --gid app --uid 10001 app

WORKDIR /app
COPY --from=builder /app/.venv /app/.venv
COPY pyproject.toml uv.lock README.md ./
COPY src ./src

USER 10001:10001
EXPOSE 8000

# The same image serves the API, the queue worker, and the cleanup job:
#   docker run ... api          (default)
#   docker run ... worker
#   docker run ... cleanup
ENTRYPOINT ["voice-recorder"]
CMD ["api", "--host", "0.0.0.0", "--port", "8000"]
