FROM python:3.12-slim
RUN apt-get update && apt-get install -y --no-install-recommends tesseract-ocr tesseract-ocr-eng tesseract-ocr-ind libgl1 libglib2.0-0 && rm -rf /var/lib/apt/lists/*
RUN pip install --no-cache-dir rapidocr==3.10.0 onnxruntime==1.31.1 openvino "onnxtr[cpu]" pytesseract "numpy<3" Pillow
COPY server.py /app/server.py
ENV OCR_PORT=8000
CMD ["python", "/app/server.py"]
