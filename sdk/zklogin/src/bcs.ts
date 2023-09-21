// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

import { BCS, fromB64, toB64 } from '@mysten/bcs';
import { bcs } from '@mysten/sui.js/bcs';
import { SIGNATURE_SCHEME_TO_FLAG } from '@mysten/sui.js/cryptography';

export const zkLoginBcs = new BCS(bcs);

type ProofPoints = {
	a: string[];
	b: string[][];
	c: string[];
};

type IssBase64 = {
	value: string;
	indexMod4: number;
};

export interface ZkLoginSignatureInputs {
	proofPoints: ProofPoints;
	issBase64Details: IssBase64;
	headerBase64: string;
	addressSeed: string;
}

export interface ZkLoginSignature {
	inputs: ZkLoginSignatureInputs;
	maxEpoch: number;
	userSignature: string | Uint8Array;
}

zkLoginBcs.registerStructType('ZkLoginSignature', {
	inputs: {
		proofPoints: {
			a: [BCS.VECTOR, BCS.STRING],
			b: [BCS.VECTOR, [BCS.VECTOR, BCS.STRING]],
			c: [BCS.VECTOR, BCS.STRING],
		},
		issBase64Details: {
			value: BCS.STRING,
			indexMod4: BCS.U8,
		},
		headerBase64: BCS.STRING,
		addressSeed: BCS.STRING,
	},
	maxEpoch: BCS.U64,
	userSignature: [BCS.VECTOR, BCS.U8],
});

function getZkLoginSignatureBytes({ inputs, maxEpoch, userSignature }: ZkLoginSignature) {
	return zkLoginBcs
		.ser(
			'ZkLoginSignature',
			{
				inputs,
				maxEpoch,
				userSignature: typeof userSignature === 'string' ? fromB64(userSignature) : userSignature,
			},
			{ maxSize: 2048 },
		)
		.toBytes();
}

export function getZkLoginSignature({ inputs, maxEpoch, userSignature }: ZkLoginSignature) {
	const bytes = getZkLoginSignatureBytes({ inputs, maxEpoch, userSignature });
	const signatureBytes = new Uint8Array(bytes.length + 1);
	signatureBytes.set([SIGNATURE_SCHEME_TO_FLAG.ZkLogin]);
	signatureBytes.set(bytes, 1);
	return toB64(signatureBytes);
}
