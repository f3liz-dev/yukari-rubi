import Browser from "webextension-polyfill"
import { createBirpc } from "birpc"
import { alignFurigana, containsKanji } from "./lib/furigana"
import type { Morpheme } from "./types"
import type { BackgroundRPC } from "./rpc"

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

// --- Glob pattern matching for URLs ---

function globToRegExp(pattern: string): RegExp {
  const escaped = pattern.replace(/[.+^${}()|[\]\\]/g, '\\$&')
  const withWildcards = escaped.replace(/\*\*/g, '\x00').replace(/\*/g, '[^/]*').replace(/\x00/g, '.*')
  return new RegExp(`^${withWildcards}$`)
}

function urlMatchesPatterns(url: string, patterns: string[]): boolean {
  return patterns.some((p) => globToRegExp(p).test(url))
}

// --- Set up birpc client ---

// For browser extensions, responses come back via sendMessage promise,
// but birpc expects them via the on() callback. We need to bridge this.
let onMessageCallback: ((data: any) => void) | null = null

const bg = createBirpc<BackgroundRPC>({}, {
  post: async (data) => {
    console.log("[yukari-rubi] Content sending birpc message:", data)
    const response = await Browser.runtime.sendMessage(data)
    console.log("[yukari-rubi] Content received birpc response:", response)
    // Feed the response back into birpc via the on() callback
    if (response && onMessageCallback) {
      onMessageCallback(response)
    }
    return response
  },
  on: (fn) => {
    onMessageCallback = fn
    Browser.runtime.onMessage.addListener((message) => {
      // Check if this is a birpc message by looking for the type field
      if (message && typeof message === 'object' && message.t) {
        console.log("[yukari-rubi] Content received birpc message:", message)
        fn(message)
      }
    })
  },
  serialize: (v) => v,
  deserialize: (v) => v,
})

// --- Tokenization via background ---

async function tokenize(text: string): Promise<readonly Morpheme[]> {
  const response = await bg.tokenize(text, "A")
  if (response.error) throw new Error(response.error)
  return response.morphemes ?? []
}

// --- DOM utilities ---

function shouldSkipElement(el: Element): boolean {
  return SKIP_TAGS.has(el.tagName) || el.classList.contains(CONTAINER_CLASS)
}

function collectTextNodes(root: Node): Text[] {
  const nodes: Text[] = []
  let skippedRuby = 0
  let skippedProcessed = 0
  let skippedNoKanji = 0
  let skippedOther = 0
  
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node: Node): number {
      const parent = node.parentElement
      if (!parent) {
        skippedOther++
        return NodeFilter.FILTER_SKIP
      }
      if (parent.closest(`.${CONTAINER_CLASS}`)) {
        return NodeFilter.FILTER_REJECT
      }
      if (parent.hasAttribute(PROCESSED_ATTR)) {
        skippedProcessed++
        return NodeFilter.FILTER_REJECT
      }
      if (shouldSkipElement(parent)) {
        if (parent.tagName === "RUBY" || parent.closest("ruby")) {
          skippedRuby++
        } else {
          skippedOther++
        }
        return NodeFilter.FILTER_REJECT
      }
      if (!node.textContent || !containsKanji(node.textContent)) {
        skippedNoKanji++
        return NodeFilter.FILTER_SKIP
      }
      return NodeFilter.FILTER_ACCEPT
    },
  })

  let current = walker.nextNode()
  // oxlint-disable-next-line fp/no-loop-statements
  while (current) {
    nodes.push(current as Text)
    current = walker.nextNode()
  }
  console.log(`[yukari-rubi] Found ${nodes.length} text nodes to process`)
  console.log(`[yukari-rubi] Skipped: ${skippedRuby} in ruby tags, ${skippedProcessed} already processed, ${skippedNoKanji} without kanji, ${skippedOther} other`)
  return nodes
}

// --- Font metrics utilities ---

function getActualTextBounds(element: HTMLElement): {
  fontAscent: number
  actualAscent: number
  fontSize: number
} {
  const style = window.getComputedStyle(element)
  const fontSize = parseFloat(style.fontSize)

  const canvas = document.createElement('canvas')
  const ctx = canvas.getContext('2d')
  if (!ctx) {
    return { fontAscent: fontSize * 0.8, actualAscent: fontSize * 0.7, fontSize }
  }

  ctx.font = `${style.fontStyle} ${style.fontWeight} ${fontSize}px ${style.fontFamily}`
  const metrics = ctx.measureText('字')

  return {
    fontAscent: metrics.fontBoundingBoxAscent ?? fontSize * 0.8,
    actualAscent: metrics.actualBoundingBoxAscent,
    fontSize,
  }
}

// --- Ruby element creation ---

function createRubyFragment(
  segments: readonly import("./lib/furigana").FuriganaSegment[],
  parentElement: HTMLElement,
): DocumentFragment {
  const frag = document.createDocumentFragment()
  
  for (const seg of segments) {
    if (seg.type === "kanji") {
      const ruby = document.createElement("ruby")
      
      // Get ACTUAL rendered bounds of this specific kanji text
      const { fontAscent, actualAscent, fontSize } = getActualTextBounds(parentElement)
      const gap = fontAscent - actualAscent - fontSize * 0.1
      ruby.style.setProperty('--ruby-adjustment', `${Math.max(gap, 0)}px`)
      console.log(`[yukari-rubi] fontSize: ${fontSize}px, actualAscent: ${actualAscent.toFixed(1)}px, adjustment: ${gap.toFixed(1)}px`)
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

  console.log(`[yukari-rubi] Processing text node: "${text.substring(0, 50)}${text.length > 50 ? "..." : ""}"`)
  const morphemes = await tokenize(text)
  console.log(`[yukari-rubi] Tokenized into ${morphemes.length} morphemes`)

  // Re-check state and node after async call
  if (!active || !textNode.parentNode || !textNode.isConnected) return

  const container = document.createElement("span")
  container.className = CONTAINER_CLASS
  container.setAttribute(PROCESSED_ATTR, text)

  const parent = textNode.parentElement || document.body

  for (const m of morphemes) {
    const segments = alignFurigana(m.surface, m.readingForm)
    if (segments) {
      container.appendChild(createRubyFragment(segments, parent))
    } else {
      container.appendChild(document.createTextNode(m.surface))
    }
  }

  // Double check parent one last time before replacing
  if (textNode.parentNode) {
    const parent = textNode.parentNode as Element
    console.log(`[yukari-rubi] Replacing text node in <${parent.nodeName}> with ${morphemes.length} morphemes`)
    textNode.parentNode.replaceChild(container, textNode)
  } else {
    console.warn("[yukari-rubi] Text node lost parent before replacement")
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

function notifyStatus(): void {
  Browser.runtime.sendMessage({ type: "statusChanged", active })
}

async function activate(): Promise<void> {
  console.log("[yukari-rubi] Activating furigana on page:", location.href)
  active = true
  notifyStatus()
  if (processing) return
  processing = true
  try {
    // Ensure background script is ready before processing
    console.log("[yukari-rubi] Preloading background script...")
    await bg.preload()
    console.log("[yukari-rubi] Processing document body...")
    await processRoot(document.body)
    const settings = await bg.getSettings()
    console.log("[yukari-rubi] Settings:", settings)
    const rubySize = settings.rubySize ?? 50
    document.documentElement.style.setProperty('--yukari-ruby-size', String(rubySize))
    if (settings.mutationObserver) {
      console.log("[yukari-rubi] Starting mutation observer...")
      startObserver()
    }
    console.log("[yukari-rubi] Activation complete")
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
  console.log("[yukari-rubi] Deactivating furigana")
  active = false
  notifyStatus()
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

// --- Live update ruby size on storage change ---

Browser.storage.onChanged.addListener((changes) => {
  if (changes.rubySize?.newValue !== undefined) {
    document.documentElement.style.setProperty('--yukari-ruby-size', String(changes.rubySize.newValue))
  }
})

// --- Message listener ---

Browser.runtime.onMessage.addListener((message: { type?: string; $birpc?: any }): Promise<unknown> | undefined => {
  // Skip birpc messages (handled by birpc client)
  if (message.$birpc) return undefined

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

// --- Auto-enable on matching URLs ---

Browser.storage.local.get(["autoEnablePatterns"]).then((result) => {
  const patterns: string[] = result.autoEnablePatterns ?? []
  if (patterns.length > 0 && urlMatchesPatterns(location.href, patterns)) {
    console.log("[yukari-rubi] URL matches auto-enable pattern, activating...")
    void activate()
  }
})
