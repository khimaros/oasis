// drives two headless chrome tabs through the page over the chrome devtools
// protocol: the board, sign up, chat, profiles, and a peer to peer file
// transfer. started by run.sh.
const [debugPort, base] = process.argv.slice(2);

async function tab(url) {
  const info = await (await fetch(`http://127.0.0.1:${debugPort}/json/new?${encodeURIComponent(url)}`, { method: 'PUT' })).json();
  const ws = new WebSocket(info.webSocketDebuggerUrl);
  await new Promise(done => { ws.onopen = done; });
  let next = 1;
  const waiting = new Map();
  ws.onmessage = event => {
    const message = JSON.parse(event.data);
    if (waiting.has(message.id)) { waiting.get(message.id)(message); waiting.delete(message.id); }
  };
  const send = (method, params = {}) => new Promise(done => {
    waiting.set(next, done);
    ws.send(JSON.stringify({ id: next++, method, params }));
  });
  const run = async expression => {
    const reply = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
    if (reply.result.exceptionDetails) throw new Error(`${expression}: ${JSON.stringify(reply.result.exceptionDetails.exception)}`);
    return reply.result.result.value;
  };
  const until = async (expression, label) => {
    for (let tries = 0; tries < 100; tries++) {
      if (await run(expression)) return;
      await new Promise(done => setTimeout(done, 200));
    }
    throw new Error(`timed out: ${label || expression}`);
  };
  await until('document.readyState === "complete" && typeof poll === "function"');
  // a first visit opens the sign up view. decline it, and skip the page
  // about bookmarking, to start from the board
  await until('greeted', 'first poll');
  await run("localStorage.saved = 1; if (tab === 'account') skip.click(); 1");
  await until("tab === 'board' && intro.hidden", 'declining sign up leads to the board');
  return { run, until };
}

const check = (label, ok) => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}`); if (!ok) process.exitCode = 1; };
const text = id => `document.getElementById('${id}').innerText`;

const a = await tab(base), b = await tab(base);

// --- main buttons ---
// the link out of android's sign-in window must look like the other main buttons
const fill = id => `getComputedStyle(${id}).backgroundColor`;
check('"open in my browser" is filled like a main button',
  await a.run(`${fill('browser')} === ${fill('saved')} && ${fill('browser')} !== ${fill('stay')}`));

// --- welcome page ---
await a.run("showIntro('welcome'); 1");
const lefts = await a.run("[...document.querySelectorAll('#welcome .feature span')].map(node => Math.round(node.getBoundingClientRect().left))");
check(`feature descriptions share one left edge (${lefts})`, lefts.length === 4 && new Set(lefts).size === 1);
await a.run('showIntro(); 1');

// --- board ---
check('topic list shows five topics with descriptions', (await a.run(text('topics'))).includes('offer, trade, lend') &&
  await a.run("document.querySelectorAll('#topics .entry').length") === 5);
await a.run("document.querySelectorAll('#topics .entry')[2].click(); 1");
await a.until("!document.getElementById('threads').hidden", 'threads view');
check('breadcrumb names the topic', await a.run(text('place')) === 'marketplace');
const right = selector => `Math.round(document.querySelector('${selector}').getBoundingClientRect().right)`;
check('the post button is right aligned', await a.run(`${right('#threadform button')} === ${right('#threadtext')}`));
await a.run("subject.value = 'bike for trade'; threadtext.value = 'red, 3 gears\\nneeds a chain'; threadform.requestSubmit(); 1");
await a.until("document.querySelector('#threadlist .entry')", 'thread appears');
check('thread list shows the subject', (await a.run(text('threadlist'))).includes('bike for trade'));
await a.run("document.querySelector('#threadlist .entry').click(); 1");
await a.until("!document.getElementById('thread').hidden && document.querySelector('#opener .entry')", 'thread view');
check('thread view shows the body', (await a.run(text('opener'))).includes('needs a chain'));
// --- navigation through the address ---
const at = (tabPage, hash) => tabPage.until(`location.hash === '${hash}'`, `address is ${hash}`);
await at(a, '#board/marketplace/1');
await a.run('history.back(); 1');
await a.until("!document.getElementById('threads').hidden && location.hash === '#board/marketplace'", 'browser back leaves the thread');
await a.run('history.forward(); 1');
await a.until("!document.getElementById('thread').hidden && document.getElementById('opener').innerText.includes('needs a chain')", 'browser forward reopens it');
check('browser back and forward move between the thread and its topic', true);
await a.run("location.hash = 'board/events'; 1");
await a.until("place.innerText === 'events' && !document.getElementById('threads').hidden", 'a typed address opens that topic');
await a.run('history.back(); 1');
await a.until("!document.getElementById('thread').hidden", 'back to the thread');
check('an address can be opened directly', true);
await a.run('who.click(); 1');
await a.until("location.hash === '#account' && document.getElementById('account').classList.contains('on')", 'account view');
check('sign up is a view with the back arrow', await a.run("getComputedStyle(back).display !== 'none' && intro.hidden"));
await a.run('back.click(); 1');
await a.until("location.hash === '#board/marketplace/1' && !document.getElementById('thread').hidden", 'arrow returns from sign up');
check('the arrow leaves sign up the way it came', true);

await a.run("replytext.value = 'i have a chain'; replyform.requestSubmit(); 1");
await a.until("document.querySelector('#replylist .entry')", 'reply appears');
check('reply is listed', (await a.run(text('replylist'))).includes('i have a chain'));
await a.run("back.click(); 1");
await a.until("!document.getElementById('threads').hidden", 'back to threads');
await a.run("back.click(); 1");
const shown = "getComputedStyle(back).display !== 'none'";
await a.until(`!topics.hidden && !(${shown})`, 'back to the topic list');
check('back twice returns to the topic list, where the arrow is gone', true);
await a.run("document.querySelectorAll('#topics .entry')[0].click(); 1");
await a.until(shown, 'the arrow appears');
check('the arrow shows inside a topic', await a.run('back.offsetWidth === back.offsetHeight && back.offsetWidth === who.offsetWidth'));
await a.run("show('chat'); 1");
check('the arrow is gone on other tabs', await a.run(`!(${shown})`));
await a.run("show('board'); 1");
check('status line is clean', await a.run("document.querySelector('header .status').textContent") === '');

// --- account ---
await a.run("who.click(); username.value = 'ada'; password.value = 'hunter22'; description.value = 'gardener'; join.requestSubmit(); 1");
await a.until("who.title === 'ada'", 'signed up');
check('signing up keeps the mail tab', await a.run("getComputedStyle(mailtab).display !== 'none'"));

// --- mail between a guest and an account ---
const guest = await b.run('me.name');
check('guests see the mail tab too', guest.startsWith('~') && await b.run("getComputedStyle(mailtab).display !== 'none'"));
await b.run("show('mail'); mailto.value = 'ada'; mailtext.value = 'hello from a guest'; mailform.requestSubmit(); 1");
await a.until("mailtab.textContent === 'mail (1)'", 'unread count shows');
await a.run("show('mail'); 1");
await a.until("maillist.innerText.includes('hello from a guest')", 'mail arrives');
check('mail names the guest who sent it', (await a.run(text('maillist'))).includes(`from ${guest}`));
await a.run("[...document.querySelectorAll('#maillist button')].find(x => x.textContent === 'reply').click(); mailtext.value = 'hello back'; mailform.requestSubmit(); 1");
await b.until("mailtab.textContent === 'mail (1)' || maillist.innerText.includes('hello back')", 'reply reaches the guest');
await b.run("show('mail'); 1");
await b.until("maillist.innerText.includes('from ada')", 'guest reads the reply');
check('the guest receives the reply', true);

// --- status ---
await a.run('info.click(); 1');
await a.until("location.hash === '#status' && stored.innerText.includes('accounts')", 'status view');
const statusText = await a.run("document.getElementById('status').innerText");
check('status shows who is online', (await a.run('online.innerText')).includes('ada'));
check('status shows what is stored and how full the storage is',
  /accounts\s+1 of 100/.test(statusText) && /threads\s+1 in 1 KB of \d+ KB/.test(statusText) && /storage\s+\d+ KB of \d+ KB/.test(statusText));
check('status bars are drawn', await a.run("document.querySelectorAll('#status .bar i').length") === 6);
check('people in the app get a green dot', await a.run("getComputedStyle(document.querySelector('#online .presence i')).backgroundColor") === 'rgb(200, 230, 176)');
await a.run('back.click(); 1');
await a.until("location.hash !== '#status'", 'arrow leaves status');

// --- notifications ---
// ada started "bike for trade". a reply by the guest in the other tab rings her bell
const news = await a.run('JSON.stringify({ notes, mail: me.mail })');
check(`the bell is quiet without news (${news})`, await a.run("!bell.classList.contains('on')"));
await b.run("show('board/marketplace/1'); 1");
await b.until("!document.getElementById('thread').hidden", 'guest opens the thread');
await b.run("replytext.value = 'would you take a stove for it?'; replyform.requestSubmit(); 1");
await a.until("bell.classList.contains('on')", 'the bell rings');
await a.run('bell.click(); 1');
await a.until("location.hash === '#notes' && notelist.innerText.includes('bike for trade')", 'notification view');
check('a reply to our thread is announced with who replied', (await a.run('notelist.innerText')).includes(`${guest} replied`));
await a.run("document.querySelector('#notelist .entry').click(); 1");
await a.until("location.hash === '#board/marketplace/1' && replylist.innerText.includes('stove')", 'the note leads to the thread');
check('opening the thread clears the notification', await a.run("!bell.classList.contains('on')"));
const ownNews = await b.run('JSON.stringify({ notes, mail: me.mail })');
check(`our own replies do not ring the bell (${ownNews})`, await b.run("!bell.classList.contains('on')"));

// --- chat and profile ---
await a.run("show('chat'); chattext.value = 'hello from ada'; chatform.requestSubmit(); 1");
await b.until("chatlog.innerText.includes('hello from ada')", 'chat reaches the other tab');
await b.run("show('chat'); [...document.querySelectorAll('#chatlog span')].find(s => s.textContent === 'ada').click(); 1");
await b.until("location.hash === '#user/ada' && profiletext.innerText === 'gardener'", 'profile opens');
check('a name opens the profile page with its description', await b.run("profilename.innerText === 'ada' && getComputedStyle(back).display !== 'none'"));
await b.run('profilemail.click(); 1');
await b.until("tab === 'mail' && mailto.value === 'ada'", 'send mail from the profile');
check('the profile has a button that starts a mail', true);
// guests have a profile too, reachable from every place that shows a name
const guestName = `[...document.querySelectorAll('#maillist .name')].find(s => s.textContent === ${JSON.stringify(guest)})`;
await a.run("show('mail'); 1");
await a.until(guestName, 'the mail list names the guest');
await a.run(`${guestName}.click(); 1`);
await a.until(`location.hash === '#user/' + encodeURIComponent(${JSON.stringify(guest)}) && profilename.innerText === ${JSON.stringify(guest)}`, 'guest profile');
check('a guest name opens a profile as well', await a.run("getComputedStyle(profilemail).display !== 'none'"));
const clickable = await a.run("show('status'); new Promise(done => setTimeout(() => done(document.querySelectorAll('#online .name').length), 600))");
check('names on the status page are links too', clickable >= 1);
await a.run("show('mail'); 1");
await a.until("document.querySelector('#maillist .name')", 'mail list');
check('names in the mail list are links too', true);

// --- peer to peer file transfer ---
await a.until('peers.length === 1', 'tab a sees tab b');
await b.until('peers.length === 1', 'tab b sees tab a');
const payload = 'oasis '.repeat(20000);
await a.run(`offerFile(peers[0].id, new File([${JSON.stringify(payload)}], 'hello.txt')); 1`);
await b.until("[...document.querySelectorAll('#transfers button')].some(x => x.textContent === 'accept')", 'offer arrives');
check('receiver is taken to the files tab', await b.run("tab === 'files'"));
await b.run("[...document.querySelectorAll('#transfers button')].find(x => x.textContent === 'accept').click(); 1");
await b.until("document.querySelector('#transfers a')", 'file received');
const received = await b.run("fetch(document.querySelector('#transfers a').href).then(r => r.text())");
check(`file arrives intact (${received.length} bytes)`, received === payload);
await a.until("transfers.innerText.includes('sent hello.txt')", 'sender sees completion');
check('sender reports the transfer as sent', true);
process.exit();
