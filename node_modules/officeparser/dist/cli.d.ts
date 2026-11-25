#!/usr/bin/env node
/**
 * officeparser CLI
 *
 * Allows running officeparser from the command line:
 *   npx officeparser file.docx
 *   officeparser file.docx --toText=true
 *   officeparser file.docx --ocr=true --extractAttachments=true
 *
 * Options (--key=value):
 *   --toText=true             Output plain text instead of JSON AST
 *   --ocr=true                Enable OCR for images
 *   --ocrLanguage=eng         OCR language (default: eng)
 *   --extractAttachments=true Extract embedded attachments
 *   --ignoreNotes=true        Ignore footnotes/endnotes
 *   --putNotesAtLast=true     Move notes to end of document
 *   --includeRawContent=true  Include raw content in AST
 *   --outputErrorToConsole=true  Log errors to console
 */
export {};
