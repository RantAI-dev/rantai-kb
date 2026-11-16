# Document OCR on a small CPU (layout + PP-OCRv6 tiny + SLANet-plus tables) behind
# the chat shape kb-server's local OCR adapter sends. Used by the lite and lite-en
# compose profiles. Every model is Apache-2.0; see server.py for the settings.
FROM python:3.12-slim

ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1
RUN apt-get update \
 && apt-get install -y --no-install-recommends libgl1 libglib2.0-0 curl \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

COPY server.py .
# Fetch every ONNX file (OCR det/rec, layout, table) at build time, so the container
# starts offline and the first page is not a download. Downloads are retried: the
# model host drops long transfers now and then.
RUN for attempt in 1 2 3 4 5; do \
      python -c "import server; server.warm_up()" </dev/null && exit 0; \
      echo "model download failed (attempt $attempt), retrying" >&2; sleep 5; \
    done; exit 1

EXPOSE 8000
HEALTHCHECK --interval=10s --timeout=5s --retries=6 --start-period=60s \
  CMD curl -fsS http://127.0.0.1:8000/health || exit 1
USER nobody
CMD ["python", "server.py"]
