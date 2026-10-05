// drives two headless chrome tabs through the page over the chrome devtools
// protocol: the board, sign up, chat, profiles, and a peer to peer file
// transfer. started by run.sh.
const [debugPort, base, adminUser, adminPassword] = process.argv.slice(2);

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
// the header buttons keep one distance: to the band above, to each other,
// and to the edge of the screen
check('the space right of the account button equals the space above and left of it', await a.run(`(() => {
  const [account, status, band] = [who, info, document.getElementById('band')].map(node => node.getBoundingClientRect());
  const spaces = [document.body.getBoundingClientRect().right - account.right, account.top - band.bottom, account.left - status.right];
  return spaces.every(space => Math.round(space) === Math.round(spaces[0]));
})()`));
// writing is behind an action button, not a form above the list
const visible = id => `getComputedStyle(${id}).display !== 'none'`;
check('the new thread form is closed until asked for', await a.run(`!(${visible('threadform')}) && ${visible('write')}`));
const spot = selector => `JSON.stringify(['left', 'top'].map(side => Math.round(document.querySelector('${selector}').getBoundingClientRect()[side])))`;
const writeSpot = await a.run(spot('#write'));
await a.run('write.click(); 1');
check('the send button takes the exact place of the action button', await a.run(spot('#threadform > button')) === writeSpot);
check('the action button gives way to the panel', await a.run(`${visible('threadform')} && !(${visible('write')})`));
const edge = (selector, side) => `Math.round(document.querySelector('${selector}').getBoundingClientRect().${side})`;
// every panel is on the grid of the chat box: half a rem around and
// between its parts, and one line fields as tall as the button
const box = id => `${id}.getBoundingClientRect()`;
check('the parts of a panel are spaced like the chat box', await a.run(`[
  ${box('subject')}.left - ${box('threadform')}.left, ${box('threadtext')}.top - ${box('subject')}.bottom,
  ${box('threadform')}.bottom - ${box("threadform.querySelector(':scope > button')")}.bottom,
  ${box("threadform.querySelector(':scope > button')")}.top - ${box('threadtext')}.bottom,
].every(space => Math.round(space) === 8) && Math.round(${box('subject')}.height) === 40`));
check('the panel is docked right above the tab bar', await a.run(`${edge('nav', 'top')} - ${edge('#threadform', 'bottom')} < 16`));
check('the panel says what it is', (await a.run("threadform.querySelector('.title').innerText")).trim() === 'new thread');
await a.run("threadform.querySelector('.title button').click(); 1");
check('the cross closes the panel and the action button is back', await a.run(`!(${visible('threadform')}) && ${visible('write')}`));
await a.run('write.click(); 1');
const sendIcon = form => `${form}.querySelector(':scope > button use, .row button use').getAttribute('href') === '#send'`;
check('the panel sends with an icon at its bottom right', await a.run(
  `${sendIcon('threadform')} && threadform.innerText.trim() === 'new thread' && ${right('#threadform > button')} === ${right('#threadtext')}`));
await a.run("subject.value = 'bike for trade'; threadtext.value = 'red, 3 gears\\nneeds a chain'; threadform.requestSubmit(); 1");
await a.until("document.querySelector('#threadlist .entry')", 'thread appears');
check('thread list shows the subject', (await a.run(text('threadlist'))).includes('bike for trade'));
check('a thread without replies has no count', await a.run("!document.querySelector('#threadlist .pill')"));
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
// the page looks the thread up again, and can only reply to it after that
await a.until("location.hash === '#board/marketplace/1' && !document.getElementById('thread').hidden && thread?.id === 1", 'arrow returns from sign up');
check('the arrow leaves sign up the way it came', true);

await a.run("replytext.value = 'i have a chain'; replyform.requestSubmit(); 1");
await a.until("document.querySelector('#replylist .entry')", 'reply appears');
check('reply is listed', (await a.run(text('replylist'))).includes('i have a chain'));
await a.run("back.click(); 1");
// a count is a pill at the right end of the first line of its card
const pill = card => `document.querySelector('${card} .pill')`;
const placed = card => `(() => { const [box, count, title, below] = ['', ' .pill', ' b', ' .meta'].map(part => document.querySelector('${card}' + part).getBoundingClientRect()); return box.right - count.right < 20 && count.left > title.left && count.bottom <= below.top + 1; })()`;
const bike = '#threadlist .entry';
await a.until(`!document.getElementById('threads').hidden && ${pill(bike)}?.textContent === '1'`, 'back to threads');
check('the thread list counts the replies of a thread', await a.run(placed(bike)));
await a.run("back.click(); 1");
const shown = "getComputedStyle(back).display !== 'none'";
await a.until(`!topics.hidden && !(${shown})`, 'back to the topic list');
const topicCard = number => `#topics .entry:nth-child(${number})`;
await a.until(`${pill(topicCard(3))}?.textContent === '1'`, 'thread count');
check('the topic list counts the threads of a topic', await a.run(placed(topicCard(3))));
check('a topic without threads has no count', await a.run(`!${pill(topicCard(1))}`));
check('back twice returns to the topic list, where the arrow is gone', true);
await a.run("document.querySelectorAll('#topics .entry')[0].click(); 1");
await a.until(shown, 'the arrow appears');
check('the arrow shows inside a topic', await a.run('back.offsetWidth === back.offsetHeight && back.offsetWidth === who.offsetWidth'));
await a.run("show('chat'); 1");
check('the arrow is gone on other tabs', await a.run(`!(${shown})`));
await a.run("show('board'); 1");
check('no dialog came up', await a.run(`!(${visible('refusal')})`));
check('posting closes the form again', await a.run(`!(${visible('threadform')})`));
check('there is no action button on the topic list or in chat', await a.run(`!(${visible('write')})`));

// --- network trouble ---
// shown by the status button turning red, not by text in the page
await a.run('window.realFetch = fetch; fetch = () => Promise.reject(new Error("offline")); 1');
await a.until("info.classList.contains('down')", 'the status button turns red');
await a.run("show('mail'); show('board'); 1");
check('a lost connection turns the status button red, without a dialog', await a.run(
  `getComputedStyle(info).backgroundColor === 'rgb(240, 138, 122)' && !(${visible('refusal')})`));
await a.run('fetch = realFetch; 1');
await a.until("!info.classList.contains('down')", 'the status button recovers');

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
const replyButton = "document.querySelector('#maillist button[title=reply]')";
check('the reply button on a mail is a filled icon', await a.run(
  `getComputedStyle(${replyButton}).backgroundColor === 'rgb(143, 195, 238)' && ${replyButton}.textContent === '' && ${replyButton}.querySelector('use').getAttribute('href') === '#reply'`));
const gap = side => `Math.round(${replyButton}.closest('.entry').getBoundingClientRect().${side} - ${replyButton}.getBoundingClientRect().${side})`;
// one distance for the page margin, the space between cards, and the
// padding of a card on every side
const cardGap = `Math.round(${replyButton}.closest('.entry').getBoundingClientRect().left - document.body.getBoundingClientRect().left)`;
const textTop = `(() => { const entry = ${replyButton}.closest('.entry'); return Math.round(entry.querySelector('.meta').getBoundingClientRect().top - entry.getBoundingClientRect().top); })()`;
check('it sits at the bottom right of the mail, as far from the edges as the text is', await a.run(
  `[${gap('right')}, ${gap('bottom')}, ${textTop}, parseFloat(getComputedStyle(${replyButton}.closest('.entry')).paddingLeft)].every(space => Math.round(space) === ${cardGap})`));
check('the tab bar is as far below the dock as cards are apart', await a.run(
  `Math.round(document.querySelector('nav').getBoundingClientRect().top - write.getBoundingClientRect().bottom) === ${cardGap} + 8`));
check('the mail form is closed until asked for', await a.run(`!(${visible('mailform')}) && ${visible('write')}`));
await a.run(`${replyButton}.click(); 1`);
check('reply opens the mail form, addressed', await a.run(`${visible('mailform')} && mailto.value === ${JSON.stringify(guest)}`));
// a refusal by the device is a small dialog that goes away on okay
await a.run("mailto.value = 'nobody-at-all'; mailtext.value = 'hello?'; mailform.requestSubmit(); 1");
await a.until(visible('refusal'), 'the refusal shows');
check('a refusal is shown in a dialog', (await a.run(text('refused'))) === 'nobody here by that name');
await a.run('okay.click(); 1');
check('okay closes the dialog and keeps the form', await a.run(`!(${visible('refusal')}) && ${visible('mailform')}`));
await a.run(`mailto.value = ${JSON.stringify(guest)}; mailtext.value = 'hello back'; mailform.requestSubmit(); 1`);
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
  /accounts\s+2 of 100/.test(statusText) && /threads\s+1 in 1 KB of \d+ KB/.test(statusText) && /storage\s+\d+ KB of \d+ KB/.test(statusText));
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
const firstNote = await a.run("document.querySelector('#notelist .entry').innerText");
check('a reply is announced on two lines: who replied where, and how it starts',
  firstNote.split('\n').length === 2 && firstNote.includes(`${guest} replied in bike for trade`) && firstNote.includes('would you take a stove for it?'));
// every reply and every mail is a notification of its own, and mail says who it is from
await b.run("replytext.value = 'or a lamp?'; replyform.requestSubmit(); 1");
await a.until("document.querySelectorAll('#notelist .entry').length === 2", 'a second reply is a second notification');
await b.run("show('mail'); mailto.value = 'ada'; mailtext.value = 'one more thing'; mailform.requestSubmit(); 1");
await a.until("document.querySelectorAll('#notelist .entry').length === 3", 'a mail is a notification');
const icons = await a.run("[...document.querySelectorAll('#notelist .entry use')].map(use => use.getAttribute('href')).sort().join()");
check(`each kind of notification has its icon (${icons})`, icons === '#bubble,#bubble,#envelope');
const mailNote = await a.run("[...document.querySelectorAll('#notelist .entry')].find(row => row.innerText.includes('mail from')).innerText");
check('a mail notification says who it is from and how it starts', mailNote === `mail from ${guest}\none more thing`);
await a.run("[...document.querySelectorAll('#notelist .entry')].find(row => row.innerText.includes('mail from')).click(); 1");
await a.until("tab === 'mail' && maillist.innerText.includes('one more thing')", 'the mail note leads to the mailbox');
await a.run("show('notes'); 1");
await a.until("document.querySelectorAll('#notelist .entry').length === 2", 'reading mail clears its notifications');
check('reading the mail clears its notification only', true);
await a.run("document.querySelector('#notelist .entry').click(); 1");
await a.until("location.hash === '#board/marketplace/1' && replylist.innerText.includes('stove')", 'the note leads to the thread');
check('opening the thread clears the notification', await a.run("!bell.classList.contains('on')"));
const ownNews = await b.run('JSON.stringify({ notes, mail: me.mail })');
check(`our own replies do not ring the bell (${ownNews})`, await b.run("!bell.classList.contains('on')"));

// --- chat and profile ---
await a.run("show('chat'); 1");
check('chat has its panel docked, with a send icon and no cross', await a.run(
  `${visible('chatform')} && ${sendIcon('chatform')} && !chatform.querySelector('.title') && ${edge('nav', 'top')} - ${edge('#chatform', 'bottom')} < 16`));
const chatSpot = await a.run(spot('#chatform button'));
await a.run("show('mail'); 1");
check('the chat panel stays on the chat tab', await a.run(`!(${visible('chatform')}) && ${visible('write')}`));
check('the action button is where the send button of chat is', await a.run(spot('#write')) === chatSpot);
await a.run("show('chat'); chattext.value = 'hello from ada'; chatform.requestSubmit(); 1");
await b.until("chatlog.innerText.includes('hello from ada')", 'chat reaches the other tab');
await b.run("show('chat'); [...document.querySelectorAll('#chatlog span')].find(s => s.textContent === 'ada').click(); 1");
await b.until("location.hash === '#user/ada' && profiletext.innerText === 'gardener'", 'profile opens');
check('the profile of someone else is not marked as the own', await b.run("getComputedStyle(profileyou).display === 'none'"));
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
const marked = "[...document.querySelectorAll('#online .presence')].filter(row => row.querySelector('.pill')).map(row => row.querySelector('.name').textContent).join()";
check('the online list marks the visitor', await a.run(marked) === 'ada');
await a.run("show('user/ada'); 1");
await a.until("profilename.innerText === 'ada' && getComputedStyle(profileyou).display !== 'none'", 'own profile says so');
check('the own profile is marked, on the line of the name and right after it', await a.run(`(() => {
  const [name, you] = [profilename, profileyou].map(node => node.getBoundingClientRect());
  return you.top >= name.top && you.bottom <= name.bottom && you.left >= name.right && you.left - name.right < 20;
})()`));
await a.run("show('mail'); 1");
await a.until("document.querySelector('#maillist .name')", 'mail list');
check('names in the mail list are links too', true);

// --- admin ---
// the built in admin logs in like anyone, here in the tab of the guest
const pinShown = "[...document.querySelectorAll('#threadlist button')].some(x => x.title === 'pin')";
// pin and delete are icons on the first line of a card, left of its count
const toolsPlaced = `(() => {
  const card = document.querySelector('#threadlist .entry'), left = node => node.getBoundingClientRect().left;
  const tools = [...card.querySelectorAll('.tool')].sort((one, other) => left(one) - left(other));
  const [pin, bin, count, below] = [...tools, card.querySelector('.pill'), card.querySelector('.meta')].map(node => node.getBoundingClientRect());
  return tools.map(tool => tool.title).join() === 'pin,delete' && tools.every(tool => tool.querySelector('svg') && !tool.textContent)
    && pin.right <= bin.left && bin.right <= count.left && bin.bottom <= below.top + 1 && pin.width < 30;
})()`;
const border = "getComputedStyle(who).borderColor";
const plain = await b.run(border);
await b.run("show('board/marketplace'); 1");
await b.until("document.querySelector('#threadlist .entry')", 'threads of the guest');
check('a guest has no pin button', !(await b.run(pinShown)));
await b.run(`who.click(); username.value = ${JSON.stringify(adminUser)}; password.value = ${JSON.stringify(adminPassword)}; login.click(); 1`);
await b.until("who.classList.contains('admin')", 'logged in as the admin');
check('the account button of an admin has another color', await b.run(border) !== plain);
await b.until(`location.hash === '#board/marketplace' && ${pinShown}`, 'pin button');
check('logging in as an admin brings up the pin button', true);
check('pin and delete are small icons left of the count', await b.run(toolsPlaced));
await b.run("document.querySelector('#threadlist .entry').click(); 1");
await b.until("document.querySelector('#replylist .tool')", 'delete icon on a reply');
check('a reply has a delete icon for an admin', await b.run("document.querySelector('#replylist .tool').title === 'delete'"));
// deleting asks first
const replyCount = "document.querySelectorAll('#replylist .entry').length";
const before = await b.run(replyCount);
await b.run("document.querySelector('#replylist .tool').click(); 1");
await b.until(visible('refusal'), 'delete asks first');
check('delete asks before it deletes',
  await b.run(`${visible('cancel')} && okay.textContent === 'delete' && refused.innerText.includes('delete this reply')`));
await b.run('cancel.click(); 1');
check('cancel keeps the reply', await b.run(`!(${visible('refusal')}) && ${replyCount} === ${before}`));
await b.run("document.querySelector('#replylist .tool').click(); 1");
await b.until(visible('refusal'), 'delete asks again');
await b.run('okay.click(); 1');
await b.until(`${replyCount} === ${before - 1}`, 'reply deleted');
check('confirming deletes the reply, and the dialog is its plain self again',
  await b.run(`!(${visible('refusal')}) && okay.textContent === 'okay' && !(${visible('cancel')})`));
await b.run("show('board/marketplace'); 1");
await b.until(`location.hash === '#board/marketplace' && ${pinShown}`, 'back on the thread list');
// a pinned thread keeps a pin that can be seen: the icon in the accent color, on no background
const look = title => `(() => { const style = getComputedStyle(document.querySelector('#threadlist .tool[title=${title}]')); return style.backgroundColor + ' ' + style.color; })()`;
const unpinnedLook = await b.run(look('pin'));
await b.run("document.querySelector('#threadlist .tool[title=pin]').click(); 1");
await b.until("document.querySelector('#threadlist .tool[title=unpin]')", 'thread pinned');
const pinnedLook = await b.run(look('unpin'));
check(`the pin of a pinned thread is an accent colored icon (${pinnedLook})`,
  pinnedLook === 'rgba(0, 0, 0, 0) rgb(143, 195, 238)' && pinnedLook !== unpinnedLook);
await b.run("document.querySelector('#threadlist .tool[title=unpin]').click(); 1");
await b.until(`!document.querySelector('#threadlist .tool[title=unpin]') && ${pinShown}`, 'thread unpinned');
await b.run("show('user/ada'); 1");
await b.until("getComputedStyle(profileadmin).display !== 'none' && profileadmin.textContent === 'make admin'", 'make admin button');
await b.run('profileadmin.click(); 1');
await b.until("profileadmin.textContent === 'remove admin'", 'ada is an admin');
check('an admin makes another account an admin from its profile', true);
await a.until("who.classList.contains('admin')", 'ada learns of it');
await a.run("show('board/marketplace'); 1");
await a.until(pinShown, 'pin button for ada');
check('the new admin gets the pin button', true);
await a.run("show('user/ada'); 1");
await a.until("profilename.innerText === 'ada'", 'own profile');
check('nobody changes their own admin bit', await a.run("getComputedStyle(profileadmin).display === 'none'"));

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
