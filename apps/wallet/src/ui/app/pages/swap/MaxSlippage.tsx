// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0
import BottomMenuLayout, { Content, Menu } from '_app/shared/bottom-menu-layout';
import { Button } from '_app/shared/ButtonUI';
import { Text } from '_app/shared/text';
import { IconTooltip } from '_app/shared/tooltip';
import { IconButton } from '_components/IconButton';
import NumberInput from '_components/number-input';
import Overlay from '_components/overlay';
import { DescriptionItem } from '_pages/approval-request/transaction-request/DescriptionList';
import { type FormValues } from '_pages/swap/utils';
import { Settings16 } from '@mysten/icons/src';
import { useField, useFormikContext } from 'formik';
import { useState } from 'react';

export function MaxSlippageContent({ setModalOpen }: { setModalOpen: (isOpen: boolean) => void }) {
	const { values } = useFormikContext<FormValues>();

	const allowedMaxSlippagePercentage = values.allowedMaxSlippagePercentage;

	return (
		<DescriptionItem
			title={
				<div className="flex gap-1 items-center">
					<Text variant="bodySmall">Max Slippage Tolerance</Text>
					<div>
						<IconTooltip tip="Slippage % is the difference between the price you expect to pay or receive for a coin when you initiate a transaction and the actual price at which the transaction is executed." />
					</div>
				</div>
			}
		>
			<div className="flex gap-1 items-center">
				<Text variant="bodySmall" color="hero-dark">
					{allowedMaxSlippagePercentage}%
				</Text>

				<IconButton
					onClick={() => {
						setModalOpen(true);
					}}
					icon={<Settings16 className="h-4 w-4 text-hero-dark" />}
				/>
			</div>
		</DescriptionItem>
	);
}

export function MaxSlippageModal({
	isOpen,
	setOpen,
}: {
	setOpen: (isOpen: boolean) => void;
	isOpen: boolean;
}) {
	const [field, meta] = useField('allowedMaxSlippagePercentage');
	const form = useFormikContext();

	return (
		<Overlay showModal={isOpen} title="Max Slippage Tolerance" closeOverlay={() => setOpen(false)}>
			<div className="flex flex-col w-full h-full">
				<BottomMenuLayout>
					<Content>
						<div>
							<div className="ml-3 mb-2.5">
								<Text variant="caption" weight="semibold" color="steel">
									your max slippage tolerance
								</Text>
							</div>
							<NumberInput
								className="border-solid border border-gray-45 text-steel-darker hover:border-steel focus:border-steel rounded-lg py-2 pr-2 pl-3"
								decimals
								placeholder="0.0"
								allowNegative={false}
								form={form}
								field={field}
								suffix=" %"
								meta={meta}
							/>
							<div className="ml-3 mt-3">
								<Text variant="pSubtitle" weight="normal" color="steel-dark">
									Slippage % is the difference between the price you expect to pay or receive for a
									coin when you initiate a transaction and the actual price at which the transaction
									is executed.
								</Text>
							</div>
						</div>
					</Content>

					<Menu stuckClass="sendCoin-cta" className="w-full px-0 pb-0 mx-0 gap-2.5">
						<Button
							type="submit"
							variant="primary"
							size="tall"
							text="Save"
							onClick={() => setOpen(false)}
						/>
					</Menu>
				</BottomMenuLayout>
			</div>
		</Overlay>
	);
}

export function MaxSlippage() {
	const [isOpen, setOpen] = useState(false);

	return (
		<>
			<MaxSlippageModal isOpen={isOpen} setOpen={setOpen} />
			<MaxSlippageContent setModalOpen={setOpen} />
		</>
	);
}
