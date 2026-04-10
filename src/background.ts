import Browser from "webextension-polyfill"
import { createBirpc } from "birpc"
import type { BackgroundRPC } from "./rpc"
import init, { loadDictionary, tokenize as tokenizeWasm, freeDictionary } from "@f3liz/sudachi-wasm"

let dictHandle: number | null = null
let initPromise: Promise<number> | null = null

// --- IndexedDB dictionary cache ---

function openDictDB(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open("yukari-rubi", 1)
    req.onupgradeneeded = () => req.result.createObjectStore("dict")
    req.onsuccess = () => resolve(req.result)
    req.onerror = () => reject(req.error)
  })
}

async function loadDictFromIDB(): Promise<Uint8Array | null> {
  try {
    const db = await openDictDB()
    return await new Promise((resolve) => {
      const tx = db.transaction("dict", "readonly")
      const req = tx.objectStore("dict").get("system_core")
      req.onsuccess = () => resolve(req.result ?? null)
      req.onerror = () => resolve(null)
    })
  } catch {
    return null
  }
}

async function saveDictToIDB(bytes: Uint8Array): Promise<void> {
  try {
    const db = await openDictDB()
    const tx = db.transaction("dict", "readwrite")
    tx.objectStore("dict").put(bytes, "system_core")
  } catch (e) {
    console.warn("[yukari-rubi] Failed to cache dictionary:", e)
  }
}

async function clearDictFromIDB(): Promise<void> {
  try {
    const db = await openDictDB()
    const tx = db.transaction("dict", "readwrite")
    tx.objectStore("dict").delete("system_core")
  } catch (e) {
    console.warn("[yukari-rubi] Failed to clear dictionary cache:", e)
  }
}

// --- Tokenizer lifecycle ---

async function ensureTokenizer(): Promise<number> {
  if (dictHandle !== null) return dictHandle
  if (initPromise) return initPromise

  initPromise = (async () => {
    console.log("[yukari-rubi] Initializing tokenizer...")
    const wasmUrl = Browser.runtime.getURL("wasm/sudachi_wasm_bg.wasm")
    await init({ module_or_path: wasmUrl })

    // Try IndexedDB cache first
    let dictBytes = await loadDictFromIDB()

    if (!dictBytes) {
      console.log("[yukari-rubi] Dictionary not found in cache, fetching from bundled file...")
      // Fall back to bundled dictionary
      const dictUrl = Browser.runtime.getURL("dict/system_core.xdic")
      const resp = await fetch(dictUrl)
      if (!resp.ok) {
        throw new Error(
          "Dictionary not found. Run 'npm run fetch-dict' to download system_core.xdic.",
        )
      }
      dictBytes = new Uint8Array(await resp.arrayBuffer())
      await saveDictToIDB(dictBytes)
      console.log("[yukari-rubi] Dictionary cached to IndexedDB.")
    } else {
      console.log("[yukari-rubi] Dictionary loaded from IndexedDB cache.")
    }

    try {
      dictHandle = loadDictionary(dictBytes)
    } catch (err) {
      // Dict from cache may be corrupt — clear it and retry with bundled file
      console.warn("[yukari-rubi] Failed to load dictionary (possibly corrupt cache), clearing cache and retrying...", err)
      await clearDictFromIDB()
      const dictUrl = Browser.runtime.getURL("dict/system_core.xdic")
      const resp = await fetch(dictUrl)
      if (!resp.ok) {
        throw new Error(
          "Dictionary not found. Run 'npm run fetch-dict' to download system_core.xdic.",
        )
      }
      dictBytes = new Uint8Array(await resp.arrayBuffer())
      await saveDictToIDB(dictBytes)
      dictHandle = loadDictionary(dictBytes)
    }
    console.log("[yukari-rubi] Tokenizer initialized successfully.")
    return dictHandle
  })().catch((err) => {
    initPromise = null
    throw err
  })

  return initPromise
}

// --- Default settings ---

Browser.runtime.onInstalled.addListener(() => {
  Browser.storage.local.get(["mutationObserver", "rubySize", "autoEnablePatterns"]).then((current) => {
    const defaults: Record<string, unknown> = {}
    if (current.mutationObserver === undefined) defaults.mutationObserver = false
    if (current.rubySize === undefined) defaults.rubySize = 50
    if (current.autoEnablePatterns === undefined) defaults.autoEnablePatterns = []
    if (Object.keys(defaults).length > 0) Browser.storage.local.set(defaults)
  })
})

// --- RPC Methods ---

const MODE_MAP: Record<string, number> = {
  A: 0, // short
  B: 1, // middle
  C: 2, // long
}

const rpcMethods: BackgroundRPC = {
  async tokenize(text: string, mode = "A") {
    try {
      const handle = await ensureTokenizer()
      const modeNum = MODE_MAP[mode] ?? 0
      const results = tokenizeWasm(handle, text, modeNum)
      
      // Convert WASM results to our Morpheme format
      // Note: TokenResult instances have getters for surface/reading/pos
      const morphemes = results.map((token: any) => ({
        surface: token.surface || "",
        dictionaryForm: token.surface || "",
        readingForm: token.reading || "",
        normalizedForm: token.surface || "",
        partOfSpeech: token.pos ? token.pos.split(",") : [],
        isOov: false,
        begin: 0, // WASM doesn't provide these
        end: 0,
      }))
      
      return { morphemes }
    } catch (err) {
      return { error: String(err instanceof Error ? err.message : err) }
    }
  },

  async getSettings() {
    return await Browser.storage.local.get(["mutationObserver", "rubySize", "autoEnablePatterns"])
  },

  async setSettings(settings: { mutationObserver?: boolean; rubySize?: number; autoEnablePatterns?: string[] }) {
    await Browser.storage.local.set(settings)
    return { ok: true }
  },

  async preload() {
    try {
      await ensureTokenizer()
      return { ready: true }
    } catch (err) {
      return { ready: false, error: String(err instanceof Error ? err.message : err) }
    }
  },
}

// --- Set up birpc ---

console.log("[yukari-rubi] Background script loaded, setting up birpc...")

// Storage for sendResponse callbacks, keyed by message ID
const pendingResponses = new Map<string, (data: any) => void>()

createBirpc<BackgroundRPC>(rpcMethods, {
  post: (data) => {
    // Extract the message ID from birpc response
    const msg = data as any
    if (msg.i && pendingResponses.has(msg.i)) {
      console.log("[yukari-rubi] Background sending response for ID:", msg.i)
      const sendResponse = pendingResponses.get(msg.i)!
      pendingResponses.delete(msg.i)
      sendResponse(data)
    }
  },
  on: (fn) => {
    Browser.runtime.onMessage.addListener((message, sender, sendResponse) => {
      // Check if this is a birpc message by looking for the type field
      if (message && typeof message === 'object' && message.t) {
        console.log("[yukari-rubi] Background received birpc message, ID:", (message as any).i)
        // Store sendResponse callback for this message ID
        if ((message as any).i) {
          pendingResponses.set((message as any).i, sendResponse)
        }
        // Let birpc process the message
        fn(message)
        return true // Keep channel open for async response
      }
      return false
    })
  },
  serialize: (v) => v,
  deserialize: (v) => v,
})

console.log("[yukari-rubi] birpc initialized.")

// --- Icon management ---

const enabledIcon = {
  16: "icons/kaguyaIcons_enabled_16.png",
  32: "icons/kaguyaIcons_enabled_32.png",
  48: "icons/kaguyaIcons_enabled_48.png",
}

const disabledIcon = {
  16: "icons/kaguyaIcons_disabled_16.png",
  32: "icons/kaguyaIcons_disabled_32.png",
  48: "icons/kaguyaIcons_disabled_48.png",
}

const browserActionApi = Browser.action ?? Browser.browserAction

function updateIcon(tabId: number, active: boolean): void {
  const path = active ? enabledIcon : disabledIcon
  browserActionApi.setIcon({ path, tabId })
}

// Listen for status changes from content script
Browser.runtime.onMessage.addListener((message, sender) => {
  if (message && message.type === "statusChanged" && sender.tab?.id !== undefined) {
    updateIcon(sender.tab.id, message.active)
  }
})

// Sync icon when switching tabs
Browser.tabs.onActivated.addListener(({ tabId }) => {
  Browser.tabs.sendMessage(tabId, { type: "getStatus" }).then(
    (status: { active?: boolean }) => updateIcon(tabId, status?.active ?? false),
    () => updateIcon(tabId, false),
  )
})

// --- Keyboard shortcut ---

Browser.commands.onCommand.addListener((command) => {
  if (command === "toggle-furigana") {
    Browser.tabs.query({ active: true, currentWindow: true }).then((tabs) => {
      const tabId = tabs[0]?.id
      if (tabId !== undefined) {
        Browser.tabs.sendMessage(tabId, { type: "toggle" })
      }
    })
  }
})
