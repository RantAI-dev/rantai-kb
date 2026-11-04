# PP-OCRv6 (RapidOCR, ONNX Runtime, CPU) behind the chat shape kb-server's
# local OCR adapter sends. Used by the lite and lite-en compose profiles.
FROM python:3.12-slim

ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1
RUN apt-get update \
 && apt-get install -y --no-install-recommends libgl1 libglib2.0-0 curl \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

COPY server.py .
# Fetch the PP-OCRv6 det/rec/cls ONNX files at build time, so the container
# starts offline and the first page is not a download.
RUN python -c "import server" </dev/null

EXPOSE 8000
HEALTHCHECK --interval=10s --timeout=5s --retries=6 --start-period=30s \
  CMD curl -fsS http://127.0.0.1:8000/health || exit 1
USER nobody
CMD ["python", "server.py"]
