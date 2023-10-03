// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

import { useActiveAccount } from '_app/hooks/useActiveAccount';
import {
	allowedSwapCoinsList,
	Coins,
	coinsMap,
	getUSDCurrency,
	useBalanceConversion,
	useSuiBalanceInUSDC,
} from '_app/hooks/useDeepBook';
import { useSortedCoinsByCategories } from '_app/hooks/useSortedCoinsByCategories';
import BottomMenuLayout, { Content, Menu } from '_app/shared/bottom-menu-layout';
import { Button } from '_app/shared/ButtonUI';
import { Heading } from '_app/shared/heading';
import { InputWithAction } from '_app/shared/InputWithAction';
import { Text } from '_app/shared/text';
import { ButtonOrLink } from '_app/shared/utils/ButtonOrLink';
import { CoinIcon } from '_components/coin-icon';
import { IconButton } from '_components/IconButton';
import Loading from '_components/loading';
import Overlay from '_components/overlay';
import { filterAndSortTokenBalances } from '_helpers';
import { useActiveAddress, useCoinsReFetchingConfig } from '_hooks';
import { DescriptionItem } from '_pages/approval-request/transaction-request/DescriptionList';
import { MaxSlippage } from '_pages/swap/MaxSlippage';
import { QuoteAssets } from '_pages/swap/QuoteAssets';
import { FEES_PERCENTAGE, initialValues, type FormValues } from '_pages/swap/utils';
import { validate } from '_pages/swap/validation';
import { useCoinMetadata, useFormatCoin } from '@mysten/core';
import { useSuiClientQuery } from '@mysten/dapp-kit';
import { ArrowDown12, ArrowRight16, ChevronDown16, Refresh16 } from '@mysten/icons';
import { MIST_PER_SUI } from '@mysten/sui.js/utils';
import BigNumber from 'bignumber.js';
import clsx from 'classnames';
import { Form, Formik, useFormikContext } from 'formik';
import { useMemo, useState } from 'react';
import { useNavigate, useSearchParams } from 'react-router-dom';

function useCoinTypeData(activeCoinType: string | null) {
	const selectedAddress = useActiveAddress();

	const { staleTime, refetchInterval } = useCoinsReFetchingConfig();

	const { data: coins, isLoading: coinsLoading } = useSuiClientQuery(
		'getAllBalances',
		{ owner: selectedAddress! },
		{
			enabled: !!selectedAddress,
			refetchInterval,
			staleTime,
			select: filterAndSortTokenBalances,
		},
	);

	const activeCoin = coins?.find(({ coinType }) => coinType === activeCoinType);
	const activeCoinBalance = activeCoin?.totalBalance;
	const [tokenBalance] = useFormatCoin(activeCoinBalance, activeCoinType);
	const coinMetadata = useCoinMetadata(activeCoinType);

	return {
		activeCoin,
		tokenBalance,
		coinMetadata,
		isLoading: coinsLoading || coinMetadata.isLoading,
	};
}

function SuiToUSD({ amount, isPayAll }: { amount: string; isPayAll: boolean }) {
	const amountAsBigInt = new BigNumber(amount);
	const { rawValue } = useSuiBalanceInUSDC(amountAsBigInt);

	return (
		<div className="text-bodySmall font-medium text-hero-darkest/40">
			{isPayAll ? '~ ' : ''}
			{getUSDCurrency(rawValue)}
		</div>
	);
}

function AssetData({
	tokenBalance,
	coinType,
	symbol,
	to,
	onClick,
	disabled,
}: {
	tokenBalance: string;
	coinType: string;
	symbol: string;
	to?: string;
	onClick?: () => void;
	disabled?: boolean;
}) {
	return (
		<DescriptionItem
			title={
				<div className="flex gap-1 items-center">
					<CoinIcon coinType={coinType} size="sm" />
					<ButtonOrLink
						disabled={disabled}
						onClick={onClick}
						to={to}
						className={clsx(
							'flex gap-1 items-center no-underline outline-none border-transparent bg-transparent p-0',
							!disabled && 'cursor-pointer',
						)}
					>
						<Heading variant="heading6" weight="semibold" color="hero-dark">
							{symbol}
						</Heading>
						{!disabled && <ChevronDown16 className="h-4 w-4 text-hero-dark" />}
					</ButtonOrLink>
				</div>
			}
		>
			{!!tokenBalance && (
				<div className="text-bodySmall font-medium text-hero-darkest/40">
					{tokenBalance} {symbol}
				</div>
			)}
		</DescriptionItem>
	);
}

function getCoinFromSymbol(symbol: string) {
	switch (symbol) {
		case 'SUI':
			return Coins.SUI;
		case 'USDC':
			return Coins.USDC;
		case 'USDT':
			return Coins.USDT;
		case 'WETH':
			return Coins.WETH;
		case 'tBTC':
			return Coins.TBTC;
		default:
			return Coins.SUI;
	}
}

function QuoteAssetSection() {
	const [isQuoteAssetOpen, setQuoteAssetOpen] = useState(false);
	const { values, isValid, setFieldValue } = useFormikContext<FormValues>();
	const [searchParams] = useSearchParams();
	const activeCoinType = searchParams.get('type');
	const { data: activeCoinData } = useCoinMetadata(activeCoinType);
	const quoteAssetType = values.quoteAssetType;
	const { tokenBalance: quoteAssetBalance, coinMetadata: quotedAssetMetaData } =
		useCoinTypeData(quoteAssetType);
	const quotedAssetSymbol = quotedAssetMetaData.data?.symbol ?? '';

	const { rawValue, averagePrice, refetch, isRefetching } = useBalanceConversion(
		new BigNumber(values.amount),
		getCoinFromSymbol(activeCoinData?.symbol ?? 'SUI'),
		getCoinFromSymbol(quotedAssetSymbol),
	);

	const averagePriceAsString = averagePrice?.toString();

	const { rawValue: rawValueQuoteToUsd } = useBalanceConversion(
		new BigNumber(rawValue || 0),
		getCoinFromSymbol(quotedAssetSymbol),
		Coins.USDC,
	);

	if (!quotedAssetMetaData.data) {
		return null;
	}

	return (
		<div
			className={clsx(
				'flex flex-col border border-hero-darkest/20 rounded-xl p-5 gap-4 border-solid',
				isValid && 'bg-sui-primaryBlue2023/10',
			)}
		>
			<QuoteAssets
				isOpen={isQuoteAssetOpen}
				setOpen={setQuoteAssetOpen}
				onRowClick={(coinType) => {
					setQuoteAssetOpen(false);
					setFieldValue('quoteAssetType', coinType);
				}}
			/>
			<AssetData
				disabled
				tokenBalance={quoteAssetBalance}
				coinType={quoteAssetType}
				symbol={quotedAssetSymbol}
				onClick={() => {
					setQuoteAssetOpen(true);
				}}
			/>
			<div
				className={clsx(
					'py-2 pr-2 pl-3 rounded-lg bg-gray-40 flex gap-2',
					isValid && 'border-solid border-hero-darkest/10',
				)}
			>
				{rawValue && !isRefetching ? (
					<>
						<Text variant="body" weight="semibold" color="steel-darker">
							{rawValue}
						</Text>
						<Text variant="body" weight="semibold" color="steel">
							{quotedAssetSymbol}
						</Text>
					</>
				) : (
					<Text variant="body" weight="semibold" color="steel">
						--
					</Text>
				)}
			</div>
			{rawValue && (
				<div className="ml-3">
					<DescriptionItem
						title={
							<Text variant="bodySmall" color="steel-dark">
								{isRefetching ? '--' : getUSDCurrency(rawValueQuoteToUsd)}
							</Text>
						}
					>
						<div className="flex gap-1 items-center">
							<Text variant="bodySmall" weight="medium" color="steel-dark">
								1 {activeCoinData?.symbol} = {isRefetching ? '--' : averagePriceAsString}{' '}
								{quotedAssetSymbol}
							</Text>
							<IconButton
								icon={<Refresh16 className="h-4 w-4 text-steel-dark hover:text-hero-dark" />}
								onClick={() => refetch()}
								loading={isRefetching}
							/>
						</div>
					</DescriptionItem>

					<div className="h-px w-full bg-hero-darkest/10 my-3" />

					<MaxSlippage />
				</div>
			)}
		</div>
	);
}

function GasFeeSection() {
	const { values, isValid } = useFormikContext<FormValues>();
	const [searchParams] = useSearchParams();

	const activeCoinType = searchParams.get('type');

	const { data: activeCoinData } = useCoinMetadata(activeCoinType);

	const amount = values.amount;

	const estimatedFess = useMemo(() => {
		if (!amount || !isValid) {
			return null;
		}

		return new BigNumber(amount).times(FEES_PERCENTAGE);
	}, [amount, isValid]);

	const estimatedFessAsBigInt = estimatedFess ? new BigNumber(estimatedFess) : null;

	// TODO: need to handle for all coins
	const { rawValue } = useSuiBalanceInUSDC(estimatedFessAsBigInt);

	const formattedEstimatedFees = getUSDCurrency(rawValue);

	return (
		<div className="flex flex-col border border-hero-darkest/20 rounded-xl p-5 gap-4 border-solid">
			<DescriptionItem
				title={
					<Text variant="bodySmall" weight="medium" color="steel-dark">
						Fees ({FEES_PERCENTAGE}%)
					</Text>
				}
			>
				<Text variant="bodySmall" weight="medium" color="steel-darker">
					{estimatedFess
						? `${estimatedFess.toLocaleString()} ${activeCoinData?.symbol} (${formattedEstimatedFees})`
						: '--'}
				</Text>
			</DescriptionItem>

			<div className="bg-gray-40 h-px w-full" />

			<DescriptionItem
				title={
					<Text variant="bodySmall" weight="medium" color="steel-dark">
						Estimated Gas Fee
					</Text>
				}
			>
				<Text variant="bodySmall" weight="medium" color="steel-darker">
					--
				</Text>
			</DescriptionItem>
		</div>
	);
}

function getSwapPageAtcText(fromSymbol: string, quoteAssetType: string) {
	const toSymbol =
		Object.entries(coinsMap).find(([key, value]) => value === quoteAssetType)?.[0] || '';

	return `Swap ${fromSymbol} to ${toSymbol}`;
}

export function SwapPageForm() {
	const navigate = useNavigate();
	const [searchParams] = useSearchParams();
	const activeAccount = useActiveAccount();
	const activeAccountAddress = activeAccount?.address;
	const { staleTime, refetchInterval } = useCoinsReFetchingConfig();

	const activeCoinType = searchParams.get('type');

	const { isLoading, tokenBalance, coinMetadata } = useCoinTypeData(activeCoinType);

	const { data: coinBalances } = useSuiClientQuery(
		'getAllBalances',
		{ owner: activeAccountAddress! },
		{
			enabled: !!activeAccountAddress,
			staleTime,
			refetchInterval,
			select: filterAndSortTokenBalances,
		},
	);

	const { recognized } = useSortedCoinsByCategories(coinBalances ?? []);

	const formattedTokenBalance = tokenBalance.replace(/,/g, '');
	const symbol = coinMetadata.data?.symbol ?? '';

	const coinDecimals = coinMetadata.data?.decimals ?? 0;
	const balanceInMist = new BigNumber(tokenBalance || '0')
		.times(MIST_PER_SUI.toString())
		.toString();

	const validationSchema = useMemo(() => {
		return validate(BigInt(balanceInMist), symbol, coinDecimals);
	}, [balanceInMist, coinDecimals, symbol]);

	const renderButtonToCoinsList = useMemo(() => {
		return (
			recognized.length > 1 &&
			recognized.some((coin) => allowedSwapCoinsList.includes(coin.coinType))
		);
	}, [recognized]);

	return (
		<Overlay showModal title="Swap" closeOverlay={() => navigate('/')}>
			<div className="flex flex-col w-full h-full">
				<Loading loading={isLoading}>
					<Formik
						initialValues={initialValues}
						onSubmit={() => {}}
						validationSchema={validationSchema}
						enableReinitialize
						validateOnMount
						validateOnChange
					>
						{({ isValid, isSubmitting, setFieldValue, values, submitForm, validateField }) => {
							const newIsPayAll = !!values.amount && values.amount === tokenBalance;

							if (values.isPayAll !== newIsPayAll) {
								setFieldValue('isPayAll', newIsPayAll);
							}

							return (
								<BottomMenuLayout>
									<Content>
										<Form autoComplete="off" noValidate>
											<div
												className={clsx(
													'flex flex-col border border-hero-darkest/20 rounded-xl pt-5 pb-6 px-5 gap-4 border-solid',
													isValid && 'bg-gradients-graph-cards',
												)}
											>
												{activeCoinType && (
													<AssetData
														disabled={!renderButtonToCoinsList}
														tokenBalance={tokenBalance}
														coinType={activeCoinType}
														symbol={symbol}
														to="/swap/base-assets"
													/>
												)}
												<InputWithAction
													type="numberInput"
													name="amount"
													placeholder="0.00"
													prefix={values.isPayAll ? '~ ' : ''}
													actionText="Max"
													actionDisabled={values.isPayAll}
													suffix={` ${symbol}`}
													actionType="button"
													allowNegative={false}
													decimals
													rounded="lg"
													dark
													onActionClicked={async () => {
														// using await to make sure the value is set before the validation
														await setFieldValue('amount', formattedTokenBalance);

														validateField('amount');
													}}
												/>

												{isValid && !!values.amount && (
													<div className="ml-3">
														<SuiToUSD amount={values.amount} isPayAll={values.isPayAll} />
													</div>
												)}
											</div>

											<div className="flex my-4 gap-3 items-center">
												<div className="bg-gray-45 h-px w-full" />
												<div className="h-3 w-3">
													<ArrowDown12 className="text-steel" />
												</div>
												<div className="bg-gray-45 h-px w-full" />
											</div>

											<QuoteAssetSection />

											<div className="mt-4">
												<GasFeeSection />
											</div>
										</Form>
									</Content>

									<Menu stuckClass="sendCoin-cta" className="w-full px-0 pb-0 mx-0 gap-2.5">
										<Button
											type="submit"
											onClick={submitForm}
											variant="primary"
											loading={isSubmitting}
											disabled={!isValid || isSubmitting}
											size="tall"
											text={getSwapPageAtcText(symbol, values.quoteAssetType)}
											after={<ArrowRight16 />}
										/>
									</Menu>
								</BottomMenuLayout>
							);
						}}
					</Formik>
				</Loading>
			</div>
		</Overlay>
	);
}
