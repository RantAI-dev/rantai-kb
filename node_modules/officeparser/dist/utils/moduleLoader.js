"use strict";
/**
 * Module Loader Utility
 *
 * Centralizes dynamic imports for ESM-only packages (like file-type and pdfjs-dist)
 * to maintain CommonJS compatibility while supporting modern ESM dependencies.
 *
 * This approach prevents TypeScript from transpiling dynamic import()
 * into require() when targeting CommonJS.
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.loadFileType = loadFileType;
exports.loadPdfJs = loadPdfJs;
const envUtils_js_1 = require("./envUtils.js");
/**
 * Dynamically loads an ESM module in a Node.js CJS context.
 *
 * @param specifier - The module specifier to load
 * @returns The loaded module
 */
async function loadNodeEsmModule(specifier) {
    // We use 'new Function' to bypass static analysis of tsc and some bundlers.
    // This ensures that Node.js sees a real 'import()' at runtime, even in a CJS file.
    return new Function('s', 'return import(s)')(specifier);
}
/**
 * Specialized loader for file-type
 */
async function loadFileType() {
    if (!envUtils_js_1.isBrowser) {
        // Node.js path: Use dynamic import wrapper for CJS compatibility
        return loadNodeEsmModule('file-type');
    }
    // Browser path: standard dynamic import() is handled by bundlers (e.g. esbuild/Vite)
    return import('file-type');
}
/**
 * Specialized loader for pdfjs-dist
 */
async function loadPdfJs() {
    if (!envUtils_js_1.isBrowser) {
        // Ensure DOMMatrix polyfill for Node.js 18 support
        (0, envUtils_js_1.ensureDomMatrix)();
        // Node.js environment: require legacy build for stability with ESM-only main
        try {
            return await loadNodeEsmModule('pdfjs-dist/legacy/build/pdf.mjs');
        }
        catch {
            return await loadNodeEsmModule('pdfjs-dist');
        }
    }
    // Browser environment: esbuild handles standard static-looking dynamic import()
    return import('pdfjs-dist');
}
