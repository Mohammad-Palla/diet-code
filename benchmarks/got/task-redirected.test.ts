import http from 'node:http';
import test from 'ava';
import got from '../source/index.js';

const startServer = async (t: any) => {
	const server = http.createServer((request, response) => {
		if (request.url === '/go') {
			response.writeHead(302, {location: '/land'});
			response.end();
			return;
		}

		response.end('landed');
	});

	await new Promise<void>(resolve => {
		server.listen(0, resolve);
	});
	t.teardown(() => {
		server.close();
	});

	const {port} = server.address() as any;
	return `http://localhost:${port}`;
};

test('response.redirected is true after a redirect', async t => {
	const prefixUrl = await startServer(t);
	const response = await got(`${prefixUrl}/go`);
	t.true(response.redirected);
	t.true(response.url.endsWith('/land'));
});

test('response.redirected is false without a redirect', async t => {
	const prefixUrl = await startServer(t);
	const response = await got(`${prefixUrl}/land`);
	t.false(response.redirected);
});

test('response.redirected works for streams', async t => {
	const prefixUrl = await startServer(t);
	const stream = got.stream(`${prefixUrl}/go`);
	const response: any = await new Promise((resolve, reject) => {
		stream.once('response', resolve);
		stream.once('error', reject);
		stream.resume();
	});
	t.true(response.redirected);
});
