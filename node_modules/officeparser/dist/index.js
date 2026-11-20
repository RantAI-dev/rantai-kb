"use strict";
/**
 * officeparser - Universal Office Document Parser
 *
 * A comprehensive Node.js library for parsing Microsoft Office and OpenDocument files
 * into structured Abstract Syntax Trees (AST) with full formatting information.
 *
 * **Supported Formats:**
 * - Microsoft Office: DOCX, XLSX, PPTX (Office Open XML)
 * - OpenDocument: ODT, ODP, ODS (ODF)
 * - Legacy: RTF (Rich Text Format)
 * - Portable: PDF
 *
 * **Key Features:**
 * - Unified AST output across all formats
 * - Rich text formatting (bold, italic, colors, fonts, etc.)
 * - Document structure (headings, lists, tables)
 * - Image extraction with optional OCR
 * - Metadata extraction
 * - TypeScript support with full type definitions
 *
 * **Quick Start:**
 * ```typescript
 * import { OfficeParser } from 'officeparser';
 *
 * const ast = await OfficeParser.parseOffice('document.docx', {
 *   extractAttachments: true,
 *   ocr: true,
 *   includeRawContent: false
 * });
 *
 * console.log(ast.toText()); // Plain text output
 * console.log(ast.content);  // Structured content tree
 * console.log(ast.metadata); // Document metadata
 * ```
 *
 * **Main Exports:**
 * - `OfficeParser` - Main parser class
 * - `OfficeParserConfig` - Configuration interface
 * - `OfficeParserAST` - AST result interface
 * - `OfficeContentNode` - Content tree node interface
 * - All type definitions
 *
 * @packageDocumentation
 * @module officeparser
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.terminateOcr = exports.parseOffice = exports.OfficeParser = void 0;
const OfficeParser_js_1 = require("./OfficeParser.js");
Object.defineProperty(exports, "OfficeParser", { enumerable: true, get: function () { return OfficeParser_js_1.OfficeParser; } });
const parseOffice = OfficeParser_js_1.OfficeParser.parseOffice;
exports.parseOffice = parseOffice;
const terminateOcr = OfficeParser_js_1.OfficeParser.terminateOcr;
exports.terminateOcr = terminateOcr;
// Default export for backward compatibility
exports.default = OfficeParser_js_1.OfficeParser;
