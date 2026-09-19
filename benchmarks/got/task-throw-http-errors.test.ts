import http from 'node:http';
import test from 'ava';
import got, {HTTPError} from '../source/index.js';

const startServer = async (t: any) => {
	const server = http.createServer((request, response) => {
		if (request.url === '/missing') {
			response.writeHead(404, {'content-type': 'text/plain'});
			response.end('not here');
			return;
		}

		if (request.url === '/boom') {
			response.writeHead(500, {'content-type': 'text/plain'});
			response.end('broken');
			return;
		}

		response.end('fine');
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

test('throwHttpErrors array: listed status resolves instead of throwing', async t => {
	const prefixUrl = await startServer(t);
	const response = await got(`${prefixUrl}/missing`, {throwHttpErrors: [404]});
	t.is(response.statusCode, 404);
	t.is(response.body, 'not here');
});

test('throwHttpErrors array: unlisted error status still throws', async t => {
	const prefixUrl = await startServer(t);
	const error = await t.throwsAsync(got(`${prefixUrl}/boom`, {throwHttpErrors: [404]}));
	t.true(error instanceof HTTPError);
	t.is((error as HTTPError).response.statusCode, 500);
});

test('throwHttpErrors boolean behavior is unchanged', async t => {
	const prefixUrl = await startServer(t);
	const error = await t.throwsAsync(got(`${prefixUrl}/missing`));
	t.true(error instanceof HTTPError);
	const response = await got(`${prefixUrl}/missing`, {throwHttpErrors: false});
	t.is(response.statusCode, 404);
});
