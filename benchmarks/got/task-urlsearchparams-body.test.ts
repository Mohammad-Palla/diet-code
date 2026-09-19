import http from 'node:http';
import test from 'ava';
import got from '../source/index.js';

const startServer = async (t: any) => {
	const seen: Array<{body: string; contentType: string | null}> = [];
	const server = http.createServer((request, response) => {
		let data = '';
		request.on('data', chunk => {
			data += chunk;
		});
		request.on('end', () => {
			seen.push({body: data, contentType: request.headers['content-type'] ?? null});
			response.end('ok');
		});
	});

	await new Promise<void>(resolve => {
		server.listen(0, resolve);
	});
	t.teardown(() => {
		server.close();
	});

	const {port} = server.address() as any;
	return {prefixUrl: `http://localhost:${port}`, seen};
};

test('URLSearchParams body is sent form-encoded with content-type', async t => {
	const {prefixUrl, seen} = await startServer(t);
	await got.post(prefixUrl, {body: new URLSearchParams({a: '1', b: 'two words'})});
	t.is(seen.length, 1);
	t.is(seen[0].body, 'a=1&b=two+words');
	t.true((seen[0].contentType ?? '').includes('application/x-www-form-urlencoded'));
});

test('explicit content-type is not overridden for URLSearchParams body', async t => {
	const {prefixUrl, seen} = await startServer(t);
	await got.post(prefixUrl, {
		body: new URLSearchParams({a: '1'}),
		headers: {'content-type': 'application/custom'},
	});
	t.is(seen.length, 1);
	t.is(seen[0].body, 'a=1');
	t.is(seen[0].contentType, 'application/custom');
});

test('string bodies still work', async t => {
	const {prefixUrl, seen} = await startServer(t);
	await got.post(prefixUrl, {body: 'plain'});
	t.is(seen.length, 1);
	t.is(seen[0].body, 'plain');
});
