"use strict";
/**
 * ZIP Archive Extraction Utilities
 *
 * Provides functions for extracting files from ZIP archives.
 * Essential for parsing OOXML (DOCX, XLSX, PPTX) and ODF (ODT, ODP, ODS) files,
 * which are all ZIP archives containing XML and media files.
 *
 * Office File Structure:
 * - DOCX: ZIP containing word/document.xml, word/styles.xml, word/media/*, etc.
 * - XLSX: ZIP containing xl/workbook.xml, xl/worksheets/sheet1.xml, etc.
 * - PPTX: ZIP containing ppt/slides/slide1.xml, ppt/media/*, etc.
 * - ODF: Similar structure with content.xml, styles.xml, etc.
 *
 * @module zipUtils
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.extractFiles = void 0;
const fflate_1 = require("fflate");
/**
 * Extracts files from a ZIP archive with optional filtering.
 *
 * This function:
 * 1. Opens the ZIP archive from a Buffer
 * 2. Iterates through all entries in the archive
 * 3. Applies a filter function to determine which files to extract
 * 4. Extracts matching files and returns them as an array
 *
 * Uses lazy entry reading for better memory efficiency with large archives.
 * Files are extracted asynchronously and collected into an array.
 *
 * @param zipInput - The ZIP file as a Node.js Buffer
 * @param filterFn - A predicate function to determine which files to extract.
 *                   Receives the filename and returns true to extract, false to skip.
 * @returns A promise resolving to an array of extracted files
 * @throws {Error} If the ZIP file cannot be opened or an entry cannot be read
 *
 * @example
 * ```typescript
 * // Extract only XML files from a DOCX
 * const files = await extractFiles(docxBuffer, (fileName) => fileName.endsWith('.xml'));
 *
 * // Extract document.xml specifically
 * const files = await extractFiles(docxBuffer, (fileName) =>
 *   fileName === 'word/document.xml'
 * );
 *
 * // Extract all files
 * const allFiles = await extractFiles(zipBuffer, () => true);
 *
 * // Extract everything except media files
 * const files = await extractFiles(zipBuffer, (fileName) =>
 *   !fileName.startsWith('word/media/')
 * );
 * ```
 *
 * @see https://pkware.cachefly.net/webdocs/casestudies/APPNOTE.TXT ZIP file format specification
 */
const extractFiles = (zipInput, filterFn) => {
    return new Promise((resolve, reject) => {
        (0, fflate_1.unzip)(new Uint8Array(zipInput.buffer, zipInput.byteOffset, zipInput.byteLength), { filter: (file) => filterFn(file.name) }, (err, decompressed) => {
            if (err)
                return reject(err);
            resolve(Object.entries(decompressed).map(([path, data]) => ({
                path,
                content: Buffer.from(data)
            })));
        });
    });
};
exports.extractFiles = extractFiles;
