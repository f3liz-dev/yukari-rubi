import { alignFurigana, containsKanji } from "./lib/furigana"
import type { Morpheme, TokenizeResponse, SettingsResponse } from "./types"

const PROCESSED_ATTR = "data-yukari"
const CONTAINER_CLASS = "yukari-rubi"

const SKIP_TAGS = new Set([
  "SCRIPT",
  "STYLE",
  "TEXTAREA",
  "INPUT",
  "SELECT",
  "RUBY",
  "RT",
  "RP",
  "CODE",
  "PRE",
  "KBD",
  "SAMP",
  "NOSCRIPT",
  "TEMPLATE",
  "SVG",
  "MATH",
  "CANVAS",
  "VIDEO",
  "AUDIO",
  "IFRAME",
  "OBJECT",
  "EMBED",
])

let active = false
let observer: MutationObserver | null = null
let processing = false

// --- Tokenization via background ---

async function sendMessageWithRetry(message: any, retries = 5, delay = 500): Promise<any> {
  // oxlint-disable-next-line fp/no-loop-statements
  for (let i = 0; i < retries; i++) {
    try {
      return await browser.runtime.sendMessage(message)
    } catch (err) {
      if (
        i < retries - 1 &&
        (err instanceof Error && err.message.includes("Could not establish connection"))
      ) {
        console.warn(`[yukari-rubi] Connection failed, retrying (${i + 1}/${retries})...`)
        await new Promise((r) => setTimeout(r, delay))
        continue
      }
      throw err
    }
  }
}

async function tokenize(text: string): Promise<readonly Morpheme[]> {
  const response: TokenizeResponse = await sendMessageWithRetry({
    type: "tokenize",
    text,
    mode: "A",
  })
  if (response.error) throw new Error(response.error)
  return response.morphemes ?? []
}

// --- DOM utilities ---

function shouldSkipElement(el: Element): boolean {
  return SKIP_TAGS.has(el.tagName) || el.classList.contains(CONTAINER_CLASS)
}

function collectTextNodes(root: Node): Text[] {
  const nodes: Text[] = []
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node: Node): number {
      const parent = node.parentElement
      if (!parent) return NodeFilter.FILTER_SKIP
      if (parent.closest(`.${CONTAINER_CLASS}`)) return NodeFilter.FILTER_REJECT
      if (parent.hasAttribute(PROCESSED_ATTR)) return NodeFilter.FILTER_REJECT
      if (shouldSkipElement(parent)) return NodeFilter.FILTER_REJECT
      if (!node.textContent || !containsKanji(node.textContent)) return NodeFilter.FILTER_SKIP
      return NodeFilter.FILTER_ACCEPT
    },
  })

  let current = walker.nextNode()
  // oxlint-disable-next-line fp/no-loop-statements
  while (current) {
    nodes.push(current as Text)
    current = walker.nextNode()
  }
  return nodes
}

// --- Ruby element creation ---

function createRubyFragment(
  segments: readonly import("./lib/furigana").FuriganaSegment[],
): DocumentFragment {
  const frag = document.createDocumentFragment()
  for (const seg of segments) {
    if (seg.type === "kanji") {
      const ruby = document.createElement("ruby")
      ruby.appendChild(document.createTextNode(seg.text))
      const rt = document.createElement("rt")
      rt.textContent = seg.reading
      ruby.appendChild(rt)
      frag.appendChild(ruby)
    } else {
      frag.appendChild(document.createTextNode(seg.text))
    }
  }
  return frag
}

// --- Process a single text node ---

async function processTextNode(textNode: Text): Promise<void> {
  const text = textNode.textContent
  if (!text || !containsKanji(text)) return
  if (!textNode.parentNode || !textNode.isConnected) return

  const morphemes = await tokenize(text)

  // Re-check state and node after async call
  if (!active || !textNode.parentNode || !textNode.isConnected) return

  const container = document.createElement("span")
  container.className = CONTAINER_CLASS
  container.setAttribute(PROCESSED_ATTR, text)

  for (const m of morphemes) {
    const segments = alignFurigana(m.surface, m.readingForm)
    if (segments) {
      container.appendChild(createRubyFragment(segments))
    } else {
      container.appendChild(document.createTextNode(m.surface))
    }
  }

  // Double check parent one last time before replacing
  if (textNode.parentNode) {
    textNode.parentNode.replaceChild(container, textNode)
  }
}

// --- Process all text nodes under root ---

async function processRoot(root: Node): Promise<void> {
  const textNodes = collectTextNodes(root)
  if (textNodes.length === 0) return

  const BATCH = 10
  for (let i = 0; i < textNodes.length; i += BATCH) {
    if (!active) break
    const batch = textNodes.slice(i, i + BATCH)
    await Promise.all(batch.map(processTextNode))
  }
}

// --- Remove all annotations ---

function removeAnnotations(): void {
  const containers = document.querySelectorAll(`.${CONTAINER_CLASS}`)
  for (const el of containers) {
    const original = el.getAttribute(PROCESSED_ATTR)
    if (original !== null) {
      const textNode = document.createTextNode(original)
      el.parentNode?.replaceChild(textNode, el)
    }
  }
}

// --- MutationObserver ---

function startObserver(): void {
  if (observer) return
  observer = new MutationObserver((mutations) => {
    if (!active) return
    for (const mutation of mutations) {
      for (const node of mutation.addedNodes) {
        try {
          if (node instanceof HTMLElement && !node.classList.contains(CONTAINER_CLASS)) {
            void processRoot(node)
          } else if (node instanceof Text && node.textContent && containsKanji(node.textContent)) {
            void processTextNode(node)
          }
        } catch (e) {
          // Ignore DeadObject errors from mutations on destroyed/navigated pages
          if (e instanceof Error && e.message.includes("DeadObject")) continue
          throw e
        }
      }
    }
  })
  if (document.body) {
    observer.observe(document.body, { childList: true, subtree: true })
  }
}

function stopObserver(): void {
  if (observer) {
    observer.disconnect()
    observer = null
  }
}

// --- Activate / Deactivate ---

async function activate(): Promise<void> {
  active = true
  if (processing) return
  processing = true
  try {
    await processRoot(document.body)
    const settings: SettingsResponse = await sendMessageWithRetry({
      type: "getSettings",
    })
    if (settings.mutationObserver) {
      startObserver()
    }
  } catch (err) {
    // DeadObject errors in Firefox occur when interacting with destroyed context
    if (err instanceof Error && err.message.includes("DeadObject")) {
      return
    }
    console.error("[yukari-rubi] activation error:", err)
  } finally {
    processing = false
  }
}

function deactivate(): void {
  active = false
  stopObserver()
  removeAnnotations()
}

async function toggle(): Promise<void> {
  if (active) {
    deactivate()
  } else {
    await activate()
  }
}

// --- Message listener ---

browser.runtime.onMessage.addListener((message: { type: string }): Promise<unknown> | undefined => {
  if (message.type === "toggle") {
    void toggle()
    return undefined
  }
  if (message.type === "activate") {
    if (!active) void activate()
    return undefined
  }
  if (message.type === "deactivate") {
    if (active) deactivate()
    return undefined
  }
  if (message.type === "getStatus") {
    return Promise.resolve({ active })
  }
  return undefined
})
