// `lib/mailview.ts`: which page a message body is drawn on, and
// `lib/mail.ts`'s `snippetText`, which keeps table borders out of a row.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

// `mailview.ts` reads `api.ts`'s mock switch, which looks at `window`.
stubBrowser({ window: globalThis, location: new URL('http://localhost/'), navigator: true })

const {
  modules: [mailview, mail, mailbody],
  close,
} = await load(['/src/lib/mailview.ts', '/src/lib/mail.ts', '/src/lib/mailbody.ts'])
const { applyBodyTheme, quoteDocument, renderBody, MAIL_CARD } = mailview
const { firstName, peopleLine, snippetText } = mail
const { estimateDesignedHeight, estimateWrittenHeight, looksDesigned, readBody } = mailbody

const DARK_PAGE = { background: MAIL_CARD.dark, text: '#eceaf0' }
const read = (html, options = {}) => readBody(html, { page: DARK_PAGE, ...options })
const shows = (html, options) => read(html, options).html

const doc = (inner) =>
  '<!DOCTYPE html><html><head><style>body{color:#1c1a17;background:#f7f6f3}' +
  '@media (prefers-color-scheme: dark){body{color:#eceaf0;background:#17161a}}</style>' +
  `</head><body>${inner}</body></html>`
const added = (html) => html.match(/<style>([^<]*)<\/style><\/head>/)?.[1] ?? ''
const options = { dark: true, showHistory: false, subject: '', imagesHidden: false }

// ── What real mail looks like, once `sanitize::sanitize` has been through it ──

const OUTLOOK_DESKTOP =
  '<style>p.MsoNormal{margin:0in;font-size:11.0pt;font-family:"Calibri",sans-serif}' +
  'a:link,span.MsoHyperlink{color:#0563C1}span.EmailStyle17{color:windowtext}</style>' +
  '<div class="WordSection1"><p class="MsoNormal"><span style="color:black">All good! ' +
  'Will send an invite shortly.</span></p><p class="MsoNormal"> </p>' +
  '<p class="MsoNormal"><span style="color:#1F497D">Josh</span></p><p class="MsoNormal"> </p>' +
  '<div style="border:none;border-top:solid #E1E1E1 1.0pt;padding:3.0pt 0in 0in 0in">' +
  '<p class="MsoNormal"><b>From: </b>Dustin Cook &lt;dustin@example.com&gt;<br>' +
  '<b>Sent: </b>Friday, October 30, 2026 4:14 PM<br><b>To: </b>Josh &lt;josh@example.com&gt;<br>' +
  '<b>Subject: </b>Re: Connecting</p></div><p class="MsoNormal">Let us go with Tuesday.</p>' +
  '<p class="MsoNormal"><img src="everyday://mail/part/m/image001.png@01DB"></p></div>'

const OUTLOOK_WEB =
  '<div style="font-family:Aptos;font-size:12pt;color:rgb(0,0,0)">Thanks for this.</div>' +
  '<div id="appendonsend"></div><hr style="display:inline-block;width:98%">' +
  '<div id="divRplyFwdMsg" dir="ltr"><b>From:</b> Dana<br><b>Sent:</b> Monday<br>' +
  '<b>To:</b> Me<br><b>Subject:</b> Hello</div><div>The earlier message.</div>'

const GMAIL =
  '<div dir="ltr">Sounds great, see you then.<br clear="all"><div>Priya</div></div><br>' +
  '<div class="gmail_quote"><div dir="ltr" class="gmail_attr">On Thu, Oct 29, 2026 at 9:00 AM ' +
  'Tom &lt;tom@example.com&gt; wrote:<br></div><blockquote class="gmail_quote" ' +
  'style="margin:0px 0px 0px 0.8ex;border-left:1px solid rgb(204,204,204)">' +
  '<div dir="ltr">Earlier words.</div></blockquote></div>'

const APPLE =
  '<div>Yes please.</div><div><br><blockquote><div>On Oct 29, 2026, at 9:00 AM, Tom ' +
  '&lt;tom@example.com&gt; wrote:</div><br><div>Earlier words.</div></blockquote></div>'

const APPLE_ATTRIBUTION_OUTSIDE =
  '<div>Yes please.</div><div><br></div><div>On Oct 29, 2026, at 9:00 AM, Tom ' +
  '&lt;tom@example.com&gt; wrote:<br></div><blockquote><div>Earlier words.</div></blockquote>'

// ── Folding the history ───────────────────────────────────────────────

for (const [name, html, own, gone] of [
  ['Outlook desktop', OUTLOOK_DESKTOP, 'All good!', 'Let us go with Tuesday'],
  ['Outlook on the web', OUTLOOK_WEB, 'Thanks for this', 'The earlier message'],
  ['Gmail', GMAIL, 'Sounds great', 'Earlier words'],
  ['Apple Mail', APPLE_ATTRIBUTION_OUTSIDE, 'Yes please', 'Earlier words'],
  ['Apple Mail, its header in the quote', APPLE, 'Yes please', 'Earlier words'],
]) {
  const folded = read(html)
  assert.ok(folded.hasHistory, `${name}: has history`)
  assert.ok(folded.html.includes(own), `${name}: the reply stays`)
  assert.ok(!folded.html.includes(gone), `${name}: the history goes`)
  assert.ok(!/(?:From:|wrote:)/.test(folded.html), `${name}: its header goes with it`)
  const open = shows(html, { showHistory: true })
  assert.ok(open.includes(gone), `${name}: asked for, the history comes back`)
}

{
  const folded = shows(OUTLOOK_DESKTOP)
  const opens = (folded.match(/<(?:div|p)\b/g) ?? []).length
  const closes = (folded.match(/<\/(?:div|p)>/g) ?? []).length
  assert.equal(opens, closes, 'a fold inside a wrapper closes what it cut through')
  assert.ok(!/<hr/.test(shows(OUTLOOK_WEB)), "the rule above Outlook's header goes too")
  assert.ok(
    !/<br>\s*<\/div>\s*$/.test(shows(APPLE_ATTRIBUTION_OUTSIDE)) && !/<br>\s*$/.test(shows(GMAIL)),
    'no blank lines are left under the reply',
  )
}

assert.equal(read('<p>Just a note.</p>').hasHistory, false, 'nothing to fold')
assert.equal(
  read('<pre>Hi</pre><details><summary>Quoted text</summary><pre>old</pre></details>').hasHistory,
  true,
  "a plain-text message's quoted tail folds",
)

{
  const bottom =
    '<div class="gmail_quote"><div class="gmail_attr">On Thu, Tom wrote:</div>' +
    '<blockquote class="gmail_quote">Earlier words.</blockquote><br>My answer is below it.</div>'
  assert.equal(read(bottom).hasHistory, false, 'a reply written under the quote is not hidden')
  assert.ok(shows(bottom).includes('My answer'))

  const inline =
    '<div>See inline.</div><blockquote type="cite">First question?</blockquote><div>First answer.' +
    '</div><blockquote type="cite">Second question?</blockquote>'
  const folded = shows(inline)
  assert.ok(folded.includes('First answer'), 'an inline answer stays')
  assert.ok(!folded.includes('Second question'), 'only what trails the last answer folds')

  const thunderbird =
    '<div class="moz-cite-prefix">Am 29.10.26 um 09:00 schrieb Tom:<br></div>' +
    '<blockquote type="cite">Earlier words.</blockquote>Mein Antwort steht hier.'
  assert.equal(read(thunderbird).hasHistory, false, 'Thunderbird posts underneath by default')

  const signature =
    '<div dir="ltr">Yes.</div><div class="gmail_extra"><div class="gmail_signature">Priya</div>' +
    '<div class="gmail_quote">On Thu, Oct 29, 2026 Tom wrote:<blockquote>Old.</blockquote></div></div>'
  assert.ok(shows(signature).includes('Priya'), 'a signature above the quote stays')
  assert.ok(!shows(signature).includes('Old.'))

  const elsewhere =
    '<div>Thanks.</div><div id="divRplyFwdMsg"><b>Von:</b> Dana<br><b>Betreff:</b> Hallo</div>' +
    '<div>Die fruehere Nachricht.</div>'
  assert.ok(
    !shows(elsewhere).includes('fruehere'),
    "Outlook's header starts the history, wherever its words are, even in a header this cannot read",
  )

  // An ordinary word in somebody's answer is not a gateway's footer.
  for (const answer of [
    'Yes. The virus scan passed, and the rest of my answer is here.',
    'Those are confidential until Friday.',
    'Privileged and confidential: my notes follow.',
    'Yes, I agree. This email is confidential and intended for you.',
  ]) {
    const below = `<p>See below.</p><blockquote type="cite">Ok?</blockquote><p>${answer}</p>`
    assert.equal(read(below).hasHistory, false, `an answer under the quote: ${answer}`)
    assert.ok(shows(below).includes(answer))
  }
  assert.equal(
    read(`${GMAIL}<div>-- <br>You received this message because you subscribed.</div>`).hasHistory,
    true,
    "a list's footer",
  )

  // A sentence can start with "On" and end in "wrote:".
  const sentence =
    '<p>Hi Sam,</p><p>On the subject of the launch, this is what I wrote:</p><p>We ship.</p>'
  assert.equal(read(sentence).hasHistory, false, 'prose is not a reply header')
  const digest = '<p>Top stories</p><p>On Monday, Seth wrote:</p><p>A thing about marketing.</p>'
  assert.equal(read(digest).hasHistory, false, 'nor is a digest')
  assert.ok(shows(digest).includes('A thing about marketing'))
  assert.equal(
    read('<p>Yes</p><p>On Thu, Tom wrote:</p><blockquote>Old.</blockquote>').hasHistory,
    true,
    'no date, but the quote it introduces is right there',
  )
  assert.equal(
    read('<p>Yes</p><p>On Oct 29, 2026, Tom wrote:</p><p>Old, in a client with no blockquote.</p>')
      .hasHistory,
    true,
    'a date says it is a header without one',
  )

  // What somebody wrote is not only letters and digits.
  assert.ok(
    shows(`<div>Thanks!</div><div>🙏</div>${GMAIL}`).includes('🙏'),
    'an emoji above a fold',
  )
  assert.ok(
    shows('<p>Yes?</p><p>?</p><blockquote type="cite">Old.</blockquote>').includes('<p>?</p>'),
    'a lone question mark',
  )
  const thumbs = '<p>See below.</p><blockquote type="cite">Ok to ship?</blockquote><p>👍</p>'
  assert.equal(read(thumbs).hasHistory, false, 'an emoji answer under the quote')
  assert.ok(
    !shows('<div>Hi</div><div>----</div><blockquote type="cite">Old.</blockquote>').includes(
      '----',
    ),
    'a rule is still a rule',
  )

  // Nothing in it, nothing to fold.
  assert.equal(read('<div>Hi there</div><div id="appendonsend"></div>').hasHistory, false)
  assert.equal(read('<div>Hi there</div><div class="gmail_quote"></div>').hasHistory, false)

  const quoteOnly = '<blockquote type="cite">Earlier words.</blockquote>'
  assert.equal(read(quoteOnly).hasHistory, false, 'nothing of their own: nothing is folded')

  const footer =
    GMAIL + '<div>This email is confidential and intended for the addressee only.</div>'
  assert.equal(
    read(footer).hasHistory,
    true,
    "a gateway's stamp under the history does not stop it",
  )
  assert.ok(!shows(footer).includes('Earlier words'))

  const forwarded =
    '<div>FYI</div><div class="gmail_quote"><div class="gmail_attr">---------- Forwarded message ' +
    '---------<br>From: Tom<br>Date: Thu<br>Subject: Hi<br>To: Me</div>The news.</div>'
  assert.equal(read(forwarded).hasHistory, false, 'a forward is shown whole')
  assert.equal(
    read(OUTLOOK_WEB, { subject: 'Fw: Hello' }).hasHistory,
    false,
    "Outlook's forward looks like its reply; the subject tells them apart",
  )
  assert.equal(read(OUTLOOK_WEB, { subject: 'RE: FW: Hello' }).hasHistory, true, 'a reply to one')
}

// ── Written, or designed ──────────────────────────────────────────────

const NEWSLETTER =
  '<table width="100%" style="max-width:560px;margin:0 auto"><tr><td>' +
  '<h1 style="color:#363737">The week in review</h1><table width="100%"><tr>' +
  '<td style="color:#757575">Our editors on what mattered.</td></tr></table>' +
  '<p style="color:#363737">A long paragraph of the news, with plenty of words in it.</p>' +
  '</td></tr></table>'

for (const [html, want, why] of [
  [OUTLOOK_DESKTOP, false, "Outlook's black spans and Word's sheet"],
  [GMAIL, false, 'a Gmail reply'],
  ['<p style="color:#333;background-color:#ffffff">Pasted text.</p>', false, 'white behind text'],
  ['<p>Hello, <span style="background-color:yellow">read this</span></p>', false, 'a highlight'],
  ['<p>Hello.</p><table style="width:450pt"><tr><td>Dana</td></tr></table>', false, 'a sign-off'],
  [
    '<div>Thanks.</div><table width="600"><tr><td><table><tr><td><img src="x" width="64"></td>' +
      '<td>Dana Lee, Director, Example Corporation, dana@example.com, +1 555 010 2030</td></tr>' +
      '</table></td></tr></table>',
    false,
    'a corporate signature, however it is built, under what somebody wrote',
  ],
  [
    '<table width="600"><tr><td>Item one with a price</td></tr><tr><td>Item two</td></tr></table>',
    true,
    'a receipt: nothing written above its table',
  ],
  [
    '<div style="display:none">Preview text for the inbox</div><table width="600"><tr><td>' +
      'The whole of the newsletter is in here</td></tr></table>',
    true,
    'a preheader nobody sees is not somebody writing',
  ],
  [NEWSLETTER, true, 'tables of a fixed width'],
  [
    '<table><tr><td bgcolor="#1a73e8">Your order has shipped today.</td></tr></table>',
    true,
    'a colour',
  ],
  [
    '<div style="background:#f0e6d2">A letter on tinted paper, at some length.</div>',
    true,
    'a tint',
  ],
  ['<div style="background-image:url(everyday://mail/img/m/t)">Words</div>', true, 'a picture'],
  ['<td background="everyday://mail/img/m/t">Words</td>', true, 'the old way to set one'],
  ['<style>@media (max-width:600px){td{display:block}}</style><p>Hi</p>', true, 'a breakpoint'],
  ['<style>@media (prefers-color-scheme:dark){p{color:#fff}}</style><p>Hi</p>', true, 'dark rules'],
  ['<style>body{background:#f4f4f4}</style><p>Hi</p>', true, 'a tinted page'],
]) {
  assert.equal(looksDesigned(html), want, why)
}

assert.equal(read(GMAIL + NEWSLETTER).designed, false, 'a reply over a newsletter is still a reply')

// ── Moving what was written onto the page ─────────────────────────────

{
  const out = shows('<p style="color:black;margin:0">Hi</p>')
  assert.ok(!/color/.test(out), 'black becomes the page text')
  assert.ok(out.includes('margin:0'), "the sender's other styles stay")

  const white = shows('<div style="background-color:#ffffff;color:#222">Pasted</div>')
  assert.ok(!/background/.test(white) && !/color/.test(white), 'white behind text goes')
  assert.ok(!/bgcolor/.test(shows('<td bgcolor="#FFFFFF">x</td>')), 'and the old way to set it')

  const navy = /color:(#[\da-f]{6})/.exec(shows('<span style="color:#1F497D">Josh</span>'))?.[1]
  assert.ok(navy && navy !== '#1f497d', 'a colour that meant something moves, rather than going')
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(navy.slice(i, i + 2), 16))
  assert.ok(b > r && b > g, 'and stays the same hue')

  const kept = shows('<span style="color:#ffcc00">Gold</span>')
  assert.ok(kept.includes('#ffcc00'), 'a colour already readable is left alone')

  const highlight = shows('<span style="background-color:yellow">read this</span>')
  assert.ok(/background-color:yellow/.test(highlight), 'a highlight stays')
  assert.ok(/color:#1c1a17/.test(highlight), 'and gets ink that shows on it')
  const onIt = shows('<div style="background:#003366"><span style="color:#000">Bar</span></div>')
  assert.ok(!/color:#000\b/.test(onIt), 'text is read against the cell it is on, not the page')
  assert.ok(/<span/.test(onIt))

  const font = shows('<font color="#000000" face="Arial" size="2">Old</font>')
  assert.ok(!/color=|face=|size=/.test(font), "the old font tag's page-level choices go")

  const fonts = shows('<p style="font-family:Calibri;font-size:11pt">Body</p>')
  assert.ok(!/font-/.test(fonts), "a client's font goes, so a thread reads as one thing")
  assert.ok(
    /font-family:Courier/.test(shows('<p style="font-family:Courier">Code</p>')),
    'code keeps its font',
  )
  assert.ok(
    /font-size:24px/.test(shows('<p style="font-size:24px">Big</p>')),
    'a size somebody chose stays',
  )

  const sheet = shows(OUTLOOK_DESKTOP)
  assert.ok(!/#0563C1/i.test(sheet), 'colours in a sheet move too')
  assert.ok(/font-family:inherit/.test(sheet))
  assert.ok(!/windowtext/.test(sheet))

  const url = '<a href="https://example.com/?color=red">x</a>'
  assert.equal(shows(url), url, 'a URL that says color= is not a colour')
  assert.equal(
    shows('<p>Background: we met last week.</p>'),
    '<p>Background: we met last week.</p>',
    'neither is the word in prose',
  )
  assert.equal(shows('<pre>color: red</pre>'), '<pre>color: red</pre>')
  const image = '<div style="background:url(data:image/png;base64,AA==);color:#fff">x</div>'
  assert.ok(looksDesigned(image), 'a semicolon inside a url is not the end of the declaration')
}

// ── Colours a stranger can name ───────────────────────────────────────

for (const hostile of ['constructor', '__proto__', 'hasOwnProperty', 'toString']) {
  for (const markup of [
    `<td bgcolor="${hostile}">hi</td>`,
    `<font color="${hostile}">hi</font>`,
    `<p style="color:${hostile};background:${hostile}">hi</p>`,
    `<style>p{color:${hostile}}</style><p>&${hostile};</p>`,
  ]) {
    assert.doesNotThrow(() => renderBody(doc(markup), options), markup)
    assert.doesNotThrow(() => applyBodyTheme(doc(markup), true), markup)
    assert.doesNotThrow(() => quoteDocument(markup, true), markup)
  }
}

{
  const inkOf = (html) => /color:(#[\da-f]{6})/.exec(shows(html))?.[1]
  assert.equal(inkOf('<font color="#ff0000">IMPORTANT</font>') !== undefined, true, 'a red font')
  assert.ok(
    /<font style="color:#[\da-f]{6}">IMPORTANT/.test(
      shows('<font color="#ff0000">IMPORTANT</font>'),
    ),
    'keeps a colour rather than losing it, when it is moved',
  )
  assert.ok(inkOf('<font color="#1F497D">Josh</font>'), 'and navy')

  assert.ok(inkOf('<p style="color:midnightblue">Dark</p>'), 'every name CSS has')
  assert.ok(
    !/hsl/.test(shows('<p style="color:hsl(0,0%,5%)">Dark</p>')),
    'hsl() is read, and near black becomes the page text',
  )
  assert.ok(
    /color:#1c1a17/.test(shows('<div style="background:lightyellow">note</div>')),
    'ink on it',
  )
  assert.ok(!/midnightblue/.test(shows('<style>.a{color:midnightblue}</style><p class="a">x</p>')))
  for (const unknown of [
    '<p style="color:lab(20% 10 10)">x</p>',
    '<p style="background-color:var(--panel)">x</p>',
    '<div style="background:oklch(0.9 0.1 90)">x</div>',
    '<style>p{color:color-mix(in srgb, red, blue)}</style><p>x</p>',
    '<font color="notacolour">x</font>',
  ]) {
    assert.equal(
      looksDesigned(unknown),
      true,
      `a colour nothing can read goes on white: ${unknown}`,
    )
  }
  for (const nothing of ['inherit', 'currentcolor', 'transparent', 'rgba(0,0,0,0)', '#0000']) {
    assert.equal(looksDesigned(`<p style="color:${nothing}">x</p>`), false, nothing)
  }
}

{
  // Lower-casing `İ` makes it longer, which must not move what is read after it.
  const text = `${'İ'.repeat(30)}<style>a{color:#fff}</style><div>hello</div><style>a{color:#000}</style><div>after</div>`
  const out = shows(text)
  assert.ok(out.includes('<div>hello</div>'), 'nothing is swallowed')
  assert.ok(!out.includes('color:#000'), 'and the second sheet is still read')
}

// ── renderBody never throws ───────────────────────────────────────────

{
  const body = doc('<p>Still here.</p>')
  const unreadable = {
    dark: true,
    showHistory: false,
    imagesHidden: true,
    get subject() {
      throw new Error('anything at all')
    },
  }
  const out = renderBody(body, unreadable)
  assert.equal(out.html, body, 'a message that cannot be read is shown as it was stored')
  assert.equal(out.paper, false)
  assert.equal(out.hidesImages, true, 'and its pictures can still be asked for')
}

// ── estimateWrittenHeight ─────────────────────────────────────────────

{
  const page = (inner) => `<html><head></head><body>\n${inner}\n</body></html>`
  const near = (inner, width, want, why) => {
    const got = estimateWrittenHeight(page(inner), width)
    assert.ok(got >= want && got <= want + 24, `${why}: wanted about ${want}, guessed ${got}`)
  }
  const long = 'word '.repeat(30).trim()
  near('<div dir="ltr">Sounds great.</div>', 520, 24, 'one line')
  near('<div>Hi,</div><div><br></div><div>Thanks</div>', 520, 72, 'a blank line counts')
  near('Hi,<br><br>Thanks', 520, 72, 'and written as two breaks')
  near(
    '<p class="MsoNormal">a</p><p class="MsoNormal">&nbsp;</p><p class="MsoNormal">b</p>',
    520,
    72,
    'Word',
  )
  near('<p>a</p><p>b</p>', 520, 78, "a paragraph's gap")
  near('<p style="margin:0">a</p><p style="margin:0">b</p>', 520, 48, 'unless taken away')
  near(`<div>${long}</div>`, 520, 72, 'a long line wraps')
  near(`<div>${long}</div>`, 1000, 48, 'less at a wider pane')
  near('<div>a</div><img height="48" width="160">', 520, 72, 'a picture says how tall it is')
  near('<div>a</div><img style="height:48px;width:160px">', 520, 72, 'in its style as well')
  near('<div>a</div><img height="200" width="1040">', 520, 124, 'scaled down to the column')
  near('<div>a</div><img>', 520, 124, 'or a guess when it does not')
  near('<pre>one\ntwo\n\nfour</pre>', 520, 111, 'preformatted lines are its own')
  near('<style>p{color:red;margin:0}</style><div>a</div>', 520, 24, 'a sheet draws nothing')
  near(
    '<div>a</div><div style="display:none">a hidden preheader</div>',
    520,
    24,
    'nor does hidden text',
  )
  near(
    '<div>a</div><div style="display:none"><img height="400"></div>',
    520,
    24,
    'or what is in it',
  )
  near(
    '<details open><summary>Quoted</summary><pre>x</pre></details>',
    520,
    39,
    'or a hidden summary',
  )
  const waiting = '<div>a</div><img src="everyday://mail/img/m/t">'.repeat(1)
  assert.ok(
    estimateWrittenHeight(page(waiting), 520, true) < 40,
    'a picture nobody has allowed yet takes no room',
  )
  assert.ok(estimateWrittenHeight(page(waiting), 520) > 100, 'once allowed, it does')
  assert.ok(
    estimateWrittenHeight(page(`<div>${'漢'.repeat(300)}</div>`), 600) >= 192,
    'characters drawn twice as wide take twice the lines',
  )
  assert.ok(
    estimateWrittenHeight(page(`<div>${'word '.repeat(200000)}</div>`), 520) <= 60000,
    'it does have a limit',
  )

  const row =
    '<tr><td><h2>A headline</h2><img height="300" width="600"><p>Some words.</p></td></tr>'
  const newsletter = page(`<table width="600">${row.repeat(30)}</table>`)
  assert.ok(estimateDesignedHeight(newsletter, 800) > 11000, 'a long newsletter is not cut short')
  assert.ok(
    estimateDesignedHeight(page('<table width="600"><tr><td>Short.</td></tr></table>'), 800) < 150,
    'and a short designed message is not given a page of white',
  )
}

// ── renderBody ────────────────────────────────────────────────────────

{
  const plain = renderBody(doc(GMAIL), options)
  assert.equal(plain.paper, false)
  assert.ok(added(plain.html).includes(`background:${MAIL_CARD.dark}`), 'on the card, dark')
  assert.ok(added(plain.html).includes('padding:0'), 'with no box inside the card')
  assert.ok(added(plain.html).includes('hr{'), "a rule is drawn in the page's own colours")
  assert.ok(!plain.html.includes('Earlier words'))
  assert.ok(plain.hasHistory)
  assert.ok(
    renderBody(doc(GMAIL), { ...options, showHistory: true }).html.includes('Earlier words'),
  )

  const light = renderBody(doc(GMAIL), { ...options, dark: false })
  assert.ok(added(light.html).includes(`background:${MAIL_CARD.light}`), 'on the card, light')
  assert.ok(added(light.html).includes('color-scheme:light'))

  for (const dark of [true, false]) {
    const styled = renderBody(
      doc(`<style>@media (prefers-color-scheme: dark){p{color:#fff}}</style>${NEWSLETTER}`),
      { ...options, dark },
    )
    assert.equal(styled.paper, true, `designed mail is on paper (dark=${dark})`)
    assert.ok(added(styled.html).includes('background:#ffffff'))
    assert.ok(styled.html.includes('color:#363737'), 'with every colour it came with')
    assert.ok(!/prefers-color-scheme\s*:\s*dark\)\{p/.test(styled.html), 'its dark rules are off')
    assert.ok(styled.html.includes('prefers-color-scheme: dark){body'), 'our own base style stays')
  }

  const plainText = renderBody(
    doc('<pre>Hi</pre><details><summary>Quoted text</summary><pre>a</pre></details><pre>b</pre>'),
    options,
  )
  assert.ok(plainText.html.includes('<details open>'), 'a quote left mid-answer is shown')

  assert.equal(renderBody('<p>no head</p>', options).html, '<p>no head</p>', 'nothing to put it in')
}

{
  const picture = '<img src="everyday://mail/img/m/t" width="200">'
  const hidden = { ...options, imagesHidden: true }
  const visible = renderBody(doc(`<p>Hi</p>${picture}`), hidden)
  assert.equal(visible.hidesImages, true, 'a hidden picture in view is offered')
  assert.ok(added(visible.html).includes('display:none'), 'and takes no room until it is allowed')
  assert.ok(
    added(visible.html).includes('img{background:#ffffff}'),
    "the message's own pictures still get their white backing, which a dark logo needs",
  )

  const inHistory = doc(`<p>Hi</p><blockquote type="cite">${picture}</blockquote>`)
  assert.equal(renderBody(inHistory, hidden).hidesImages, false, 'folded out of sight: not offered')
  assert.equal(renderBody(inHistory, { ...hidden, showHistory: true }).hidesImages, true)
  assert.equal(renderBody(doc(picture), options).hidesImages, false, 'allowed: nothing to offer')
  assert.ok(added(renderBody(doc(picture), options).html).includes('img{background:#ffffff}'))
}

// ── applyBodyTheme ────────────────────────────────────────────────────

const plain = doc('<pre>Hi,\nsee you then.</pre>')
const styled = doc(`<style>@media (prefers-color-scheme: dark){p{color:#fff}}</style>${NEWSLETTER}`)

assert.match(
  added(applyBodyTheme(plain, true)),
  /background:#17161a/,
  'plain mail, dark: dark page',
)
assert.match(
  added(applyBodyTheme(plain, false)),
  /background:#f7f6f3/,
  'plain mail, light: light page',
)
assert.match(
  added(applyBodyTheme(plain, false)),
  /color-scheme:light/,
  'light is written out, not left to a query about the system',
)
for (const dark of [true, false]) {
  const out = applyBodyTheme(styled, dark)
  assert.match(added(out), /background:#ffffff/, `styled mail is on white (dark=${dark})`)
  assert.match(added(out), /color:#1c1a17/)
  assert.ok(!/prefers-color-scheme\s*:\s*dark\)\{p/.test(out), "the sender's dark rules are off")
  assert.ok(out.includes('prefers-color-scheme: dark){body'), 'our own base style is untouched')
}
assert.equal(applyBodyTheme('<p>no head</p>', true), '<p>no head</p>', 'nothing to put it in')
assert.ok(
  applyBodyTheme(plain, true).includes('<pre>Hi,\nsee you then.</pre>'),
  'the body is left as it was',
)
assert.ok(
  !/color:#363737/.test(applyBodyTheme(doc('<p style="color:#363737">A note.</p>'), true)),
  'a note in dark grey is moved onto the page, not sent to a white one',
)

// ── quoteDocument ─────────────────────────────────────────────────────

assert.match(quoteDocument(NEWSLETTER, true), /background:#ffffff/)
assert.match(quoteDocument('<p>x</p>', true), /background:transparent/)
assert.match(quoteDocument('<p style="color:#333">x</p>', true), /background:transparent/)
assert.ok(!quoteDocument('<p style="color:#000">x</p>', true).includes('color:#000'))

// ── firstName and peopleLine ──────────────────────────────────────────

{
  const person = (name, email = 'x@example.com') => ({ name, email })
  assert.equal(firstName(person('Josh Grischkan')), 'Josh')
  assert.equal(firstName(person('Cook, Dustin')), 'Dustin', 'Outlook directory order')
  assert.equal(firstName(person('"Dana Lee"')), 'Dana', 'quotes a header kept')
  assert.equal(firstName(person('', 'priya@example.com')), 'priya', 'no name given')
  assert.equal(firstName(person('The Economist')), 'The Economist', 'not a person')
  assert.equal(firstName(person('Dr. Lee')), 'Dr. Lee', 'a title is not a name')
  assert.equal(firstName(person('Stripe, Inc.')), 'Stripe', 'a company, not Inc.')
  assert.equal(firstName(person('Jane Doe, MD')), 'Jane', 'a letter after a name')
  assert.equal(firstName(person('John Smith, Jr.')), 'John')
  assert.equal(firstName(person('Bank of America')), 'Bank of America')
  assert.equal(firstName(person('Google Calendar')), 'Google Calendar')

  const me = person('Hari G', 'me@example.com')
  const own = new Set(['me@example.com'])
  assert.equal(peopleLine([person('John Rosenthal'), me], own), 'John & Me')
  assert.equal(peopleLine([me, person('John R')], own), 'John & Me', 'the reader goes last')
  assert.equal(peopleLine([person('John R')], own), 'John')
  assert.equal(peopleLine([me], own), 'Me')
  assert.equal(peopleLine([person('Ana B'), person('Cal D'), me], own), 'Ana, Cal & Me')
  assert.equal(
    peopleLine(
      ['A', 'B', 'C', 'D', 'E', 'F'].map((n) => person(n)),
      own,
    ),
    'A, B, C, D +2',
  )
  assert.equal(peopleLine([], own), '')
}

// ── snippetText ───────────────────────────────────────────────────────

const rule = '─'.repeat(199)
assert.equal(snippetText(`${rule}…`), '', 'a border and nothing else is no preview')
assert.equal(snippetText(`${rule} Pre-war oil│READ IN APP`), 'Pre-war oil READ IN APP')
assert.equal(snippetText('Thanks — see you then…'), 'Thanks — see you then…', 'dashes stay')
assert.equal(snippetText(''), '')

for (const [stored, want, why] of [
  [
    'Josh, Let us go with Tuesday. Dustin **From: **Josh Grischkan <josh@example.com> **Sent: **Fri',
    'Josh, Let us go with Tuesday. Dustin',
    "the shape in Hari's inbox",
  ],
  ['Fine. From: Tom <t@e.com> Sent: Monday To: Me', 'Fine.', 'a plain Outlook header'],
  ['Fine. On Thu, Oct 29, Tom <t@e.com> wrote: earlier', 'Fine.', 'a Gmail header'],
  ['Fine. -----Original Message----- earlier', 'Fine.', 'the older marker'],
  ['Thanks **so** much', 'Thanks so much', 'bold markers go wherever they are'],
  ['From: here, the road is long', 'From: here, the road is long', 'the word on its own stays'],
  ['I was on holiday when you wrote: lovely', 'I was on holiday when you wrote: lovely', 'prose'],
]) {
  assert.equal(snippetText(stored), want, why)
}

await close()
console.log('mailview: all checks passed')
